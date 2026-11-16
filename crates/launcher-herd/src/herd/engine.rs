//! The agreement engine, one per network (docs/HERD.md, Agreement). Sans-IO: a round starts
//! with the nodes' facts ([`Engine::begin`]), asks for probes and takes their answers
//! ([`Engine::answer`]) until it needs no more, then classifies the nodes and the herd
//! ([`Engine::finish`]). It has no clock of its own: durations come from [`Now::mono`], and
//! [`Now::wall`] only judges how old the statistics are and detects a wake.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, SystemTime},
};

use launcher_types::{
    ExitClass, Level, Network, NodeId, NodeKind, NodeProgram, NodeStats, Phase, STALE_AFTER,
    SYNCED_PROGRESS,
    herd::{
        BlockRef, Branch, ExcludedWhy, Family, HerdState, HerdView, NodeInHerd, Relation, Side,
        Vote,
    },
};

use super::{
    roles::{family, vote},
    search::{Answers, Fork, Outcome, Placement, Probe, ProbeResult, fork_search, place},
    spacing::{
        DEBOUNCE, MAX_REORG, PROBATION, SHORT_PROBATION, SpacingMeter, Thresholds, target_spacing,
    },
    verdict,
};

/// The most probes one round asks; the rest wait for the next round.
pub const MAX_PROBES: usize = 64;

/// A round this much later by the wall clock than by the monotonic one followed a sleep.
const WAKE_SLACK: Duration = Duration::from_secs(30);

/// A node not answering this long wants a look.
const UNKNOWN_ATTENTION: Duration = Duration::from_secs(30);

/// A node whose probes failed for two rounds is set aside this long, so the others are
/// placed without it, and then asked again.
const ASIDE_FOR: Duration = Duration::from_secs(30);

/// What the engine needs to know of one node for a round.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberFacts {
    pub id: NodeId,
    pub name: String,
    pub kind: NodeKind,
    pub program: NodeProgram,
    pub phase: Phase,
    /// Its newest statistics, with [`NodeStats::best`].
    pub stats: Option<NodeStats>,
}

/// The time of a round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Now {
    /// A monotonic clock, for every duration.
    pub mono: Duration,
    /// The wall clock, for the statistics' age and wake detection.
    pub wall: SystemTime,
}

/// What the engine remembers of a node between rounds.
#[derive(Clone, Debug, Default)]
struct Memory {
    tip: Option<BlockRef>,
    /// When its tip last changed, and the main branch's top height then.
    tip_since: Duration,
    main_at_tip: u32,
    /// Since when it has been more than the tolerance behind.
    behind_since: Option<Duration>,
    /// Probation: when this run was first seen, whether it was already within tolerance
    /// then, since when it has been within tolerance, and whether it is through.
    seen_at: Option<Duration>,
    short_probation: bool,
    near_since: Option<Duration>,
    through: bool,
    /// It ran with fresh statistics this session: a start is then a restart.
    ran: bool,
    /// It has had fresh statistics in its current run.
    in_run: bool,
    failed_rounds: u32,
    /// Not placed until then: its probes kept failing.
    aside_until: Option<Duration>,
    unknown_since: Option<Duration>,
    shown: Option<Relation>,
    /// A state waiting for the debounce: its kind, since when, and in how many rounds.
    pending: Option<(&'static str, Duration, u32)>,
}

/// Where a node goes this round, before placement.
#[derive(Clone, Debug, PartialEq)]
enum Lane {
    Out(ExcludedWhy),
    /// It suddenly reports the genesis block.
    Genesis,
    /// Syncing, or far below while its tip moves: one placement read.
    Catching(BlockRef, f64),
    /// Placed among the others.
    Near(BlockRef, f64),
    /// Its probes kept failing: not placed for a while, so it holds up no one.
    Aside,
}

#[derive(Clone, Debug)]
struct Member {
    id: NodeId,
    name: String,
    vote: Vote,
    family: Family,
    lane: Lane,
    tip: Option<BlockRef>,
}

#[derive(Clone, Debug, Default)]
struct Round {
    now: Option<Now>,
    members: Vec<Member>,
    asked: BTreeSet<Probe>,
}

/// What the answers so far say.
#[derive(Clone, Debug, Default)]
struct Analysis {
    placement: Placement,
    /// For each branch but the main one (index 0), where it splits from the main one, once
    /// found.
    forks: Vec<Option<Fork>>,
    /// For each catching-up node, whether its tip is on the main branch.
    checks: BTreeMap<NodeId, Option<bool>>,
    needs: Vec<Probe>,
}

/// The engine of one network's herd.
#[derive(Clone, Debug)]
pub struct Engine {
    network: Network,
    answers: Answers,
    round: Round,
    memory: BTreeMap<NodeId, Memory>,
    /// The last fork found for each pair of branch tops, tried first next round.
    forks: BTreeMap<(NodeId, NodeId), u32>,
    /// When each competing branch, by its fork height, was first seen.
    branch_since: BTreeMap<u32, Duration>,
    meter: SpacingMeter,
    /// The main branch's top and when its height last grew.
    top: Option<(BlockRef, Duration)>,
    /// No herd stall before the grace has passed since this time: the first round or a wake.
    grace_from: Option<Duration>,
    last: Option<Now>,
}

impl Engine {
    pub fn new(network: Network) -> Self {
        Engine {
            network,
            answers: Answers::default(),
            round: Round::default(),
            memory: BTreeMap::new(),
            forks: BTreeMap::new(),
            branch_since: BTreeMap::new(),
            meter: SpacingMeter::default(),
            top: None,
            grace_from: None,
            last: None,
        }
    }

    pub fn network(&self) -> Network {
        self.network
    }

    /// Starts a round with the nodes' `facts` at `now` and returns the first probes.
    pub fn begin(&mut self, facts: Vec<MemberFacts>, now: Now) -> Vec<Probe> {
        self.wake_check(now);
        self.grace_from.get_or_insert(now.mono);
        let top_height = self.top.as_ref().map(|(top, _)| top.height);
        let mut members: Vec<Member> = facts
            .into_iter()
            .filter(|fact| vote(fact.kind) != Vote::None)
            .map(|fact| {
                let memory = self.memory.entry(fact.id.clone()).or_default();
                let lane = lane_of(&fact, memory, now);
                let lane = match lane {
                    Lane::Near(..) if memory.aside_until.is_some_and(|until| now.mono < until) => {
                        Lane::Aside
                    }
                    lane => lane,
                };
                let tip = match &lane {
                    Lane::Catching(tip, _) | Lane::Near(tip, _) => Some(tip.clone()),
                    Lane::Out(_) | Lane::Genesis | Lane::Aside => None,
                };
                match &lane {
                    Lane::Catching(..) | Lane::Near(..) => {
                        memory.ran = true;
                        memory.in_run = true;
                        if memory.tip.as_ref() != tip.as_ref() {
                            memory.tip_since = now.mono;
                            memory.main_at_tip =
                                top_height.unwrap_or_else(|| tip.as_ref().map_or(0, |t| t.height));
                        }
                    }
                    Lane::Out(why) => {
                        // A run that ended starts its probation again.
                        if !matches!(why, ExcludedWhy::NoStats | ExcludedWhy::NotCounted) {
                            memory.in_run = false;
                        }
                        memory.seen_at = None;
                        memory.near_since = None;
                        memory.through = false;
                        memory.behind_since = None;
                    }
                    Lane::Genesis | Lane::Aside => {}
                }
                Member {
                    id: fact.id,
                    name: fact.name,
                    vote: vote(fact.kind),
                    family: family(fact.kind, fact.program),
                    lane,
                    tip,
                }
            })
            .collect();
        // Far below the others while its tip moves: catching up, unless it is stuck.
        let highest = members
            .iter()
            .filter_map(|member| member.tip.as_ref().map(|tip| tip.height))
            .max()
            .unwrap_or(0);
        for member in &mut members {
            if let Lane::Near(tip, progress) = &member.lane {
                let memory = &self.memory[&member.id];
                let moving = memory.tip_since == now.mono;
                let stuck = matches!(memory.shown, Some(Relation::Stalled { .. }));
                if highest - tip.height > MAX_REORG && moving && !stuck {
                    member.lane = Lane::Catching(tip.clone(), *progress);
                }
            }
        }
        for member in &members {
            let memory = self.memory.get_mut(&member.id).expect("added above");
            if member.tip.is_some() {
                memory.tip.clone_from(&member.tip);
            }
        }
        let present: BTreeSet<NodeId> = members.iter().map(|member| member.id.clone()).collect();
        self.memory.retain(|id, _| present.contains(id));
        self.answers.begin(members.iter().filter_map(|member| {
            member
                .tip
                .as_ref()
                .map(|tip| (member.id.clone(), tip.clone()))
        }));
        self.round = Round {
            now: Some(now),
            members,
            asked: BTreeSet::new(),
        };
        self.step()
    }

    /// Takes answers to this round's probes and returns the next ones; none when the round
    /// can finish. Answers to probes this round did not ask are ignored.
    pub fn answer(&mut self, answers: Vec<(Probe, ProbeResult)>) -> Vec<Probe> {
        for (probe, result) in answers {
            if self.round.asked.contains(&probe) {
                self.answers.record(probe, result);
            }
        }
        self.step()
    }

    /// Runs a whole round, answering each probe with `ask`: for the browser mock and tests.
    pub fn round(
        &mut self,
        facts: Vec<MemberFacts>,
        now: Now,
        mut ask: impl FnMut(&Probe) -> ProbeResult,
    ) -> HerdView {
        let mut probes = self.begin(facts, now);
        while !probes.is_empty() {
            let answers = probes
                .into_iter()
                .map(|probe| {
                    let result = ask(&probe);
                    (probe, result)
                })
                .collect();
            probes = self.answer(answers);
        }
        self.finish()
    }

    fn step(&mut self) -> Vec<Probe> {
        let analysis = self.analyse();
        let mut next = Vec::new();
        for probe in analysis.needs {
            if self.round.asked.len() >= MAX_PROBES {
                break;
            }
            if self.round.asked.insert(probe.clone()) {
                next.push(probe);
            }
        }
        next
    }

    fn analyse(&self) -> Analysis {
        let near: Vec<(NodeId, BlockRef)> = self
            .round
            .members
            .iter()
            .filter_map(|member| match &member.lane {
                Lane::Near(tip, _) => Some((member.id.clone(), tip.clone())),
                _ => None,
            })
            .collect();
        let placement = place(&self.answers, &near);
        let mut analysis = Analysis {
            needs: placement.needs.clone(),
            placement,
            ..Analysis::default()
        };
        if !analysis.needs.is_empty() {
            return analysis;
        }
        let Some(main) = analysis.placement.branches.first().cloned() else {
            return analysis;
        };
        analysis.forks.push(None);
        for branch in analysis.placement.branches.iter().skip(1) {
            let hi = main.top.height.min(branch.top.height);
            let hint = self
                .forks
                .get(&pair(&main.top_node, &branch.top_node))
                .copied();
            match fork_search(&self.answers, &main.top_node, &branch.top_node, hi, hint) {
                Outcome::Done(fork) => analysis.forks.push(Some(fork)),
                Outcome::Need(probes) => {
                    analysis.needs.extend(probes);
                    analysis.forks.push(None);
                }
                Outcome::Failed => analysis.forks.push(None),
            }
        }
        for member in &self.round.members {
            if let Lane::Catching(tip, _) = &member.lane {
                let same = if tip.height > main.top.height {
                    None
                } else {
                    match self.answers.holds(&main.top_node, tip) {
                        Outcome::Done(same) => Some(same),
                        Outcome::Need(probes) => {
                            analysis.needs.extend(probes);
                            None
                        }
                        Outcome::Failed => None,
                    }
                };
                analysis.checks.insert(member.id.clone(), same);
            }
        }
        analysis
    }

    /// Classifies the nodes and the herd with what the round found.
    pub fn finish(&mut self) -> HerdView {
        let Some(now) = self.round.now else {
            return HerdView::off(self.network, SystemTime::UNIX_EPOCH);
        };
        let analysis = self.analyse();
        let branches = &analysis.placement.branches;
        let main = branches.first();

        // The main top, and the spacing the time rules use.
        if let Some(main) = main {
            match &mut self.top {
                Some((top, since)) if main.top.height > top.height => {
                    self.meter.record(
                        now.mono.saturating_sub(*since),
                        main.top.height - top.height,
                    );
                    *top = main.top.clone();
                    *since = now.mono;
                }
                Some((top, _)) => *top = main.top.clone(),
                None => self.top = Some((main.top.clone(), now.mono)),
            }
        }
        let reference = self.top.as_ref().map(|(top, _)| top.height).or_else(|| {
            self.round
                .members
                .iter()
                .filter_map(|member| member.tip.as_ref().map(|tip| tip.height))
                .max()
        });
        let target = target_spacing(self.network, reference.unwrap_or(0));
        let rules = Thresholds::new(self.network, self.meter.effective(target));

        // Probation, against the top of each node's own branch.
        let branch_of = |id: &NodeId| {
            branches
                .iter()
                .position(|branch| branch.members.iter().any(|(member, _)| member == id))
        };
        for member in &self.round.members {
            let memory = self
                .memory
                .get_mut(&member.id)
                .expect("every member is remembered");
            let Lane::Near(tip, _) = &member.lane else {
                continue;
            };
            let Some(index) = branch_of(&member.id) else {
                continue;
            };
            let within = branches[index].top.height - tip.height <= rules.tolerance;
            if memory.seen_at.is_none() {
                memory.seen_at = Some(now.mono);
                memory.short_probation = within;
            }
            if within {
                let since = *memory.near_since.get_or_insert(now.mono);
                let needed = if memory.short_probation {
                    SHORT_PROBATION
                } else {
                    PROBATION
                };
                if now.mono.saturating_sub(since) >= needed {
                    memory.through = true;
                }
            } else {
                memory.near_since = None;
            }
        }

        // The splits: which side each real fork's nodes are on.
        let at_tip = self
            .round
            .members
            .iter()
            .filter(|member| {
                member.vote == Vote::Witness
                    && matches!(member.lane, Lane::Near(..))
                    && self.memory[&member.id].through
            })
            .count();
        let mut splits: Vec<Option<Split>> = vec![None; branches.len()];
        if let Some(main) = main {
            for (index, branch) in branches.iter().enumerate().skip(1) {
                let Some(fork) = analysis.forks.get(index).copied().flatten() else {
                    continue;
                };
                let since = *self.branch_since.entry(fork.at).or_insert(now.mono);
                let side_len = branch.top.height + 1 - fork.at;
                let main_len = main.top.height + 1 - fork.at;
                let longer = side_len.max(main_len);
                let race = longer <= rules.race_blocks
                    || (longer == rules.race_blocks + 1
                        && now.mono.saturating_sub(since) < rules.race_for);
                let above = |placed: &super::search::Placed| -> Vec<NodeId> {
                    placed
                        .members
                        .iter()
                        .filter(|(id, height)| *height >= fork.at && self.memory[id].through)
                        .map(|(id, _)| id.clone())
                        .collect()
                };
                let (main_side, other_side) = (above(main), above(branch));
                let families = |ids: &[NodeId]| -> Vec<Family> {
                    ids.iter()
                        .filter_map(|id| self.member(id))
                        .map(|member| member.family)
                        .collect()
                };
                let (main_families, other_families) = (families(&main_side), families(&other_side));
                let disjoint = main_families
                    .iter()
                    .all(|family| !other_families.contains(family));
                let side = if !main_side.is_empty() && !other_side.is_empty() && disjoint {
                    Side::Implementations
                } else if main_side.len() > at_tip / 2 {
                    Side::Minority
                } else {
                    Side::NoMajority
                };
                splits[index] = Some(Split {
                    fork,
                    race,
                    side,
                    side_len,
                    main_len,
                    main_side,
                });
            }
        }
        let live_forks: BTreeSet<u32> =
            splits.iter().flatten().map(|split| split.fork.at).collect();
        self.branch_since.retain(|at, _| live_forks.contains(at));
        // The main branch's nodes on the far side of a split nobody wins.
        let contested: Vec<&Split> = splits
            .iter()
            .flatten()
            .filter(|split| !split.race && split.side != Side::Minority)
            .collect();

        // Each node's state this round, then debounced.
        let main_top = main.map(|branch| branch.top.height);
        let mut nodes = Vec::new();
        for member in self.round.members.clone() {
            let failed = self.answers.failed_for(&member.id);
            let memory = self.memory.get_mut(&member.id).expect("remembered");
            match &member.lane {
                Lane::Aside => {}
                _ if failed => {
                    memory.failed_rounds += 1;
                    if memory.failed_rounds >= 2 {
                        memory.aside_until = Some(now.mono + ASIDE_FOR);
                    }
                }
                Lane::Near(..) => {
                    memory.failed_rounds = 0;
                    memory.aside_until = None;
                }
                _ => {}
            }
            let raw = match &member.lane {
                Lane::Out(why) => Some(Relation::Excluded { why: *why }),
                Lane::Genesis | Lane::Aside => Some(Relation::Unknown),
                Lane::Catching(_, progress) => Some(Relation::CatchingUp {
                    progress: *progress,
                    same_chain: analysis.checks.get(&member.id).copied().flatten(),
                }),
                Lane::Near(tip, progress) => match branch_of(&member.id) {
                    _ if memory.failed_rounds >= 2 => Some(Relation::Unknown),
                    None => None,
                    Some(_) if !memory.through => Some(Relation::CatchingUp {
                        progress: *progress,
                        same_chain: Some(branch_of(&member.id) == Some(0)),
                    }),
                    Some(0) => {
                        let main_top = main_top.unwrap_or(tip.height);
                        match contested
                            .iter()
                            .find(|split| split.main_side.contains(&member.id))
                        {
                            Some(split) => Some(Relation::Forked {
                                at: split.fork.at,
                                own: split.main_len,
                                main: split.side_len,
                                side: split.side,
                                deep: split.fork.deep,
                            }),
                            None => behind(memory, tip.height, main_top, false, &rules, now),
                        }
                    }
                    Some(index) => match &splits[index] {
                        None => None,
                        Some(split) if tip.height < split.fork.at => behind(
                            memory,
                            tip.height,
                            main_top.unwrap_or(0),
                            false,
                            &rules,
                            now,
                        ),
                        Some(split) if split.race => Some(Relation::Racing { at: split.fork.at }),
                        Some(split) => {
                            let own = tip.height + 1 - split.fork.at;
                            if own >= 2 {
                                Some(Relation::Forked {
                                    at: split.fork.at,
                                    own,
                                    main: split.main_len,
                                    side: split.side,
                                    deep: split.fork.deep,
                                })
                            } else {
                                // One block the others replaced: judged like a node at the
                                // fork point, and until it is stuck it keeps its state.
                                match behind(
                                    memory,
                                    split.fork.at.saturating_sub(1),
                                    main_top.unwrap_or(0),
                                    true,
                                    &rules,
                                    now,
                                ) {
                                    Some(stalled @ Relation::Stalled { .. }) => Some(stalled),
                                    _ => None,
                                }
                            }
                        }
                    },
                },
            };
            let shown = settle(memory, raw, now.mono);
            if matches!(shown, Relation::Unknown) {
                memory.unknown_since.get_or_insert(now.mono);
            } else {
                memory.unknown_since = None;
            }
            let level = node_level(&shown, memory, &rules, now.mono);
            let (word, sentence) = verdict::describe(&member.name, &shown);
            nodes.push(NodeInHerd {
                id: member.id.clone(),
                name: member.name.clone(),
                vote: member.vote,
                family: member.family,
                tip: member.tip.clone(),
                relation: shown,
                level,
                word,
                sentence,
            });
        }

        // Remember the forks for the next round.
        if let Some(main) = main {
            for (index, branch) in branches.iter().enumerate().skip(1) {
                if let Some(fork) = analysis.forks.get(index).copied().flatten() {
                    self.forks
                        .insert(pair(&main.top_node, &branch.top_node), fork.at);
                }
            }
        }
        if self.forks.len() > 64 {
            self.forks.clear();
        }

        // The herd.
        let witnesses: Vec<&NodeInHerd> = nodes
            .iter()
            .filter(|node| node.vote == Vote::Witness)
            .collect();
        let at_tip_nodes: Vec<&NodeInHerd> = witnesses
            .iter()
            .copied()
            .filter(|node| {
                !matches!(
                    node.relation,
                    Relation::Excluded { .. } | Relation::CatchingUp { .. } | Relation::Unknown
                )
            })
            .collect();
        let running: Vec<&NodeInHerd> = witnesses
            .iter()
            .copied()
            .filter(|node| match node.relation {
                Relation::Excluded { why } => why == ExcludedWhy::Restarting,
                _ => true,
            })
            .collect();
        let quiet_for = self
            .top
            .as_ref()
            .map_or(Duration::ZERO, |(_, since)| now.mono.saturating_sub(*since));
        let past_grace = self
            .grace_from
            .is_some_and(|from| now.mono.saturating_sub(from) >= rules.grace);
        let (state, level) = if witnesses.len() < 2 {
            (HerdState::Off, Level::Neutral)
        } else if at_tip_nodes.is_empty() {
            (HerdState::Checking, Level::Neutral)
        } else if nodes
            .iter()
            .any(|node| matches!(node.relation, Relation::Forked { .. }))
        {
            (HerdState::Split, Level::Problem)
        } else if past_grace && quiet_for >= rules.herd_stall {
            let problem = rules
                .herd_stall_problem
                .is_some_and(|after| quiet_for >= after);
            (
                HerdState::Stalled,
                if problem {
                    Level::Problem
                } else {
                    Level::Attention
                },
            )
        } else if at_tip_nodes.len() == 1 {
            (HerdState::Single, Level::Neutral)
        } else if running.iter().filter(|node| agrees(node)).count() < running.len() {
            let worst = running
                .iter()
                .filter(|node| !agrees(node))
                .map(|node| node.level)
                .max()
                .unwrap_or(Level::Neutral);
            (HerdState::Degraded, worst.max(Level::Neutral))
        } else {
            (HerdState::Agreed, Level::Healthy)
        };
        let top = main.map(|branch| branch.top.clone());
        let view_branches: Vec<Branch> = branches
            .iter()
            .enumerate()
            .map(|(index, branch)| {
                let fork = analysis.forks.get(index).copied().flatten();
                Branch {
                    fork_at: fork.map(|fork| fork.at),
                    deep: fork.is_some_and(|fork| fork.deep),
                    top: branch.top.clone(),
                    nodes: branch.members.iter().map(|(id, _)| id.clone()).collect(),
                }
            })
            .collect();
        let headline = verdict::headline(&verdict::Herd {
            network: self.network,
            state,
            top: top.as_ref(),
            nodes: &nodes,
            branches: &view_branches,
            quiet_for,
        });
        let view = HerdView {
            network: self.network,
            state,
            level,
            top,
            spacing_secs: secs(target),
            effective_spacing_secs: secs(rules.spacing),
            headline,
            branches: view_branches,
            nodes,
            updated_at: now.wall,
        };
        self.last = Some(now);
        view
    }

    fn member(&self, id: &NodeId) -> Option<&Member> {
        self.round.members.iter().find(|member| &member.id == id)
    }

    /// A round much later by the wall clock than by the monotonic one followed a sleep: the
    /// timers start again, and the herd gets its grace.
    fn wake_check(&mut self, now: Now) {
        let Some(last) = self.last else {
            return;
        };
        let wall = now.wall.duration_since(last.wall).unwrap_or_default();
        let mono = now.mono.saturating_sub(last.mono);
        if wall > mono + WAKE_SLACK {
            self.grace_from = Some(now.mono);
            if let Some((_, since)) = &mut self.top {
                *since = now.mono;
            }
            for memory in self.memory.values_mut() {
                memory.tip_since = now.mono;
                memory.behind_since = None;
            }
        }
    }
}

/// A real or racing split of one branch from the main one.
#[derive(Clone, Debug)]
struct Split {
    fork: Fork,
    race: bool,
    side: Side,
    side_len: u32,
    main_len: u32,
    /// The main branch's nodes through probation above the fork.
    main_side: Vec<NodeId>,
}

/// Whether a node agrees with the main branch: on it within the tolerance, or racing.
pub(super) fn agrees(node: &NodeInHerd) -> bool {
    matches!(
        node.relation,
        Relation::InAgreement { .. } | Relation::Racing { .. }
    )
}

fn pair(a: &NodeId, b: &NodeId) -> (NodeId, NodeId) {
    if a <= b {
        (a.clone(), b.clone())
    } else {
        (b.clone(), a.clone())
    }
}

fn secs(duration: Duration) -> u32 {
    u32::try_from(duration.as_secs()).unwrap_or(u32::MAX)
}

/// Where a node goes this round, from its facts.
fn lane_of(fact: &MemberFacts, memory: &Memory, now: Now) -> Lane {
    if vote(fact.kind) != Vote::Witness {
        return Lane::Out(ExcludedWhy::NotCounted);
    }
    let start = if memory.ran {
        ExcludedWhy::Restarting
    } else {
        ExcludedWhy::Starting
    };
    match &fact.phase {
        Phase::Running => {}
        Phase::Starting | Phase::Backoff { .. } => return Lane::Out(start),
        Phase::Stopping => return Lane::Out(ExcludedWhy::Stopping),
        Phase::Failed {
            class: ExitClass::EosHalt,
        } => return Lane::Out(ExcludedWhy::EndOfSupport),
        Phase::Failed { .. } | Phase::Stopped => return Lane::Out(ExcludedWhy::Stopped),
    }
    // A sample from a clock that runs ahead counts as fresh.
    let fresh = |stats: &NodeStats| match now.wall.duration_since(stats.updated_at) {
        Ok(age) => age <= STALE_AFTER,
        Err(_) => true,
    };
    let Some((tip, progress)) = fact
        .stats
        .as_ref()
        .filter(|stats| fresh(stats))
        .and_then(|stats| Some((stats.best.clone()?, stats.verification_progress)))
    else {
        // A run that has not answered yet is still starting.
        return Lane::Out(if memory.in_run {
            ExcludedWhy::NoStats
        } else {
            start
        });
    };
    if tip.height == 0 && memory.tip.as_ref().is_some_and(|known| known.height > 0) {
        return Lane::Genesis;
    }
    if progress < SYNCED_PROGRESS {
        Lane::Catching(tip, progress)
    } else {
        Lane::Near(tip, progress)
    }
}

/// The state of a node on the main branch at `height` below its top `main_top`: agreeing
/// within the tolerance, stalled when its tip has not moved for the stall time while the top
/// gained 3 blocks, lagging when more than the tolerance behind for the lag time, and
/// otherwise still agreeing while its word shows how far behind it is.
fn behind(
    memory: &mut Memory,
    height: u32,
    main_top: u32,
    replaced_block: bool,
    rules: &Thresholds,
    now: Now,
) -> Option<Relation> {
    let blocks = main_top.saturating_sub(height);
    if blocks <= rules.tolerance && !replaced_block {
        memory.behind_since = None;
        return Some(Relation::InAgreement { behind: blocks });
    }
    let since = *memory.behind_since.get_or_insert(now.mono);
    let unchanged = now.mono.saturating_sub(memory.tip_since);
    if unchanged >= rules.stall && main_top.saturating_sub(memory.main_at_tip) >= 3 {
        return Some(Relation::Stalled {
            at: if replaced_block { height + 1 } else { height },
            replaced_block,
        });
    }
    if blocks > rules.tolerance && now.mono.saturating_sub(since) >= rules.lag {
        return Some(Relation::Lagging { blocks });
    }
    Some(Relation::InAgreement { behind: blocks })
}

/// The kind of a relation, for the debounce.
fn kind(relation: &Relation) -> &'static str {
    match relation {
        Relation::Excluded { .. } => "excluded",
        Relation::CatchingUp { .. } => "catching_up",
        Relation::InAgreement { .. } => "in_agreement",
        Relation::Racing { .. } => "racing",
        Relation::Lagging { .. } => "lagging",
        Relation::Stalled { .. } => "stalled",
        Relation::Forked { .. } => "forked",
        Relation::Unknown => "unknown",
    }
}

/// The state a node shows: a new kind of state only once it held for [`DEBOUNCE`] across
/// two rounds, except a node leaving or entering Excluded, leaving probation (which was its
/// debounce) or not answering (which took two rounds already). `raw` `None` keeps the
/// state.
fn settle(memory: &mut Memory, raw: Option<Relation>, now: Duration) -> Relation {
    let Some(raw) = raw else {
        return memory.shown.clone().unwrap_or(Relation::Excluded {
            why: ExcludedWhy::NoStats,
        });
    };
    let at_once = match &memory.shown {
        None => true,
        Some(shown) => {
            kind(shown) == kind(&raw)
                || matches!(
                    shown,
                    Relation::Excluded { .. } | Relation::CatchingUp { .. }
                )
                || matches!(raw, Relation::Excluded { .. } | Relation::Unknown)
        }
    };
    if at_once {
        memory.pending = None;
        memory.shown = Some(raw.clone());
        return raw;
    }
    let (rounds, since) = match memory.pending {
        Some((pending, since, rounds)) if pending == kind(&raw) => (rounds + 1, since),
        _ => (1, now),
    };
    if rounds >= 2 && now.saturating_sub(since) >= DEBOUNCE {
        memory.pending = None;
        memory.shown = Some(raw.clone());
        return raw;
    }
    memory.pending = Some((kind(&raw), since, rounds));
    memory.shown.clone().expect("checked above")
}

/// How grave a node's state is.
fn node_level(relation: &Relation, memory: &Memory, rules: &Thresholds, now: Duration) -> Level {
    match relation {
        Relation::InAgreement { .. } | Relation::Racing { .. } => Level::Healthy,
        Relation::Excluded { .. } => Level::Neutral,
        Relation::CatchingUp {
            same_chain: Some(false),
            ..
        } => Level::Problem,
        Relation::CatchingUp { .. } => Level::Neutral,
        Relation::Lagging { blocks } => {
            let long = memory
                .behind_since
                .is_some_and(|since| now.saturating_sub(since) >= rules.lag_problem);
            if *blocks > rules.lag_problem_blocks && long {
                Level::Problem
            } else {
                Level::Attention
            }
        }
        Relation::Stalled { .. } => {
            let stuck = now.saturating_sub(memory.tip_since);
            if rules.stall_problem.is_some_and(|after| stuck >= after) {
                Level::Problem
            } else {
                Level::Attention
            }
        }
        Relation::Forked { .. } => Level::Problem,
        Relation::Unknown => {
            if memory
                .unknown_since
                .is_some_and(|since| now.saturating_sub(since) >= UNKNOWN_ATTENTION)
            {
                Level::Attention
            } else {
                Level::Neutral
            }
        }
    }
}
