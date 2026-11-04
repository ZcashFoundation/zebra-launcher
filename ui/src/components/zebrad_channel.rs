//! Zebra's bundled release channels: the add-node choice and the compact channel labels used
//! wherever a node is named. A node's channel is chosen once, before its database is created.

use launcher_types::{NodeProgram, ZebradChannel};
use leptos::prelude::*;

/// Whether a channel is available under the version pins and the browser mock's optional Beta
/// override. Tauri passes `false` for `beta_unavailable`, matching the real backend's pin-only guard.
pub fn channel_available(channel: ZebradChannel, beta_unavailable: bool) -> bool {
    channel.is_available() && !(channel == ZebradChannel::Beta && beta_unavailable)
}

/// Selects an available channel, leaving the current choice alone when `requested` is not
/// available. In particular, an unavailable Beta never turns a draft back into Stable.
pub fn select_channel(
    current: ZebradChannel,
    requested: ZebradChannel,
    beta_unavailable: bool,
) -> ZebradChannel {
    select_channel_if(
        current,
        requested,
        channel_available(requested, beta_unavailable),
    )
}

fn select_channel_if(
    current: ZebradChannel,
    requested: ZebradChannel,
    available: bool,
) -> ZebradChannel {
    if available { requested } else { current }
}

/// Why a channel cannot be picked, if it cannot.
pub fn availability_note(channel: ZebradChannel, beta_unavailable: bool) -> Option<String> {
    availability_note_if(channel, channel_available(channel, beta_unavailable))
}

fn availability_note_if(channel: ZebradChannel, available: bool) -> Option<String> {
    if available {
        return None;
    }
    Some(match channel {
        ZebradChannel::Beta => format!(
            "Beta {} is not newer than Stable {}, so Beta cannot be selected.",
            channel.version(),
            ZebradChannel::Stable.version()
        ),
        _ => format!(
            "{} is not available in this build of Zeal.",
            channel.label()
        ),
    })
}

/// The database and maturity warning for a prerelease channel.
pub fn channel_warning(channel: ZebradChannel) -> Option<&'static str> {
    match channel {
        ZebradChannel::Stable => None,
        ZebradChannel::Beta => Some(
            "Experimental preview. Its database may use a newer format that Stable cannot open. The channel cannot be changed after this node is created.",
        ),
        ZebradChannel::Nightly => Some(
            "Experimental development build; expect breaking changes. Its database may use a newer format that Stable cannot open. The channel cannot be changed after this node is created.",
        ),
    }
}

/// The version and identifying build metadata shown with a channel choice.
pub fn channel_build(channel: ZebradChannel) -> String {
    match channel {
        ZebradChannel::Stable => format!("zebrad {}", channel.version()),
        ZebradChannel::Beta | ZebradChannel::Nightly => format!(
            "zebrad {} · {} · {}",
            channel.version(),
            channel.short_commit(),
            channel.date()
        ),
    }
}

/// A chip's full identifying metadata, including channels whose compact text hides it.
pub fn channel_tooltip(channel: ZebradChannel) -> String {
    format!(
        "{} · zebrad {} · commit {} · {}",
        channel.label(),
        channel.version(),
        channel.short_commit(),
        channel.date()
    )
}

#[component]
pub fn ZebradChannelChip(channel: ZebradChannel) -> impl IntoView {
    view! {
        <span
            class=format!("chip channel-chip channel-{}", channel.as_str())
            title=channel_tooltip(channel)
        >
            {channel.label()}
        </span>
    }
}

/// Native radios provide the arrow-key behavior and disabled semantics of the channel group.
#[component]
pub fn ZebradChannelPicker(
    selected: RwSignal<ZebradChannel>,
    beta_unavailable: bool,
) -> impl IntoView {
    view! {
        <div class="channel-picker">
            <div class="channel-options" role="radiogroup" aria-labelledby="add-node-channel">
                {ZebradChannel::ALL
                    .into_iter()
                    .map(|channel| {
                        let available = channel_available(channel, beta_unavailable);
                        let description_id =
                            format!("add-node-channel-{}-description", channel.as_str());
                        let note = availability_note(channel, beta_unavailable);
                        let guidance = channel_warning(channel).unwrap_or(
                            "The tested release bundled with Zeal, recommended for everyday use.",
                        );
                        view! {
                            <label
                                class="channel-option"
                                class:selected=move || selected.get() == channel
                                class:unavailable=!available
                            >
                                <input
                                    type="radio"
                                    name="add-node-zebrad-channel"
                                    value=channel.as_str()
                                    prop:checked=move || selected.get() == channel
                                    disabled=!available
                                    aria-describedby=description_id.clone()
                                    on:change=move |_| {
                                        selected.update(|current| {
                                            *current = select_channel(*current, channel, beta_unavailable);
                                        });
                                    }
                                />
                                <span class="channel-option-body" id=description_id>
                                    <span class="channel-option-heading">
                                        <strong>{channel.label()}</strong>
                                        {(channel == ZebradChannel::Stable)
                                            .then(|| view! { <span class="chip">"Recommended"</span> })}
                                    </span>
                                    <span class="channel-option-version">{channel_build(channel)}</span>
                                    <span class="channel-option-guidance">{guidance}</span>
                                    {note.map(|note| {
                                        view! { <span class="channel-option-unavailable">{note}</span> }
                                    })}
                                </span>
                            </label>
                        }
                    })
                    .collect_view()}
            </div>
            <p class="muted form-note">
                "The channel and its database stay with this node; use a separate node to try another channel."
            </p>
        </div>
    }
}

/// The program stored in an add-node draft, kept here as a small pure mapping for UI tests.
pub fn draft_program(channel: ZebradChannel) -> NodeProgram {
    NodeProgram::from_channel(channel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_choices_are_selected_and_unavailable_ones_are_not_reset() {
        for requested in ZebradChannel::ALL {
            assert_eq!(
                channel_available(requested, false),
                requested.is_available()
            );
            if requested.is_available() {
                assert_eq!(
                    select_channel(ZebradChannel::Stable, requested, false),
                    requested
                );
            }
            for current in ZebradChannel::ALL {
                assert_eq!(
                    select_channel_if(current, requested, false),
                    current,
                    "an unavailable choice must preserve {current:?}"
                );
            }
        }
    }

    #[test]
    fn the_mock_beta_override_disables_only_beta() {
        for channel in ZebradChannel::ALL {
            assert_eq!(
                channel_available(channel, true),
                channel != ZebradChannel::Beta && channel.is_available()
            );
        }
        for current in ZebradChannel::ALL {
            assert_eq!(
                select_channel(current, ZebradChannel::Beta, true),
                current,
                "forced-unavailable Beta must preserve {current:?}"
            );
            assert_eq!(
                select_channel(current, ZebradChannel::Nightly, true),
                ZebradChannel::Nightly
            );
        }
    }

    #[test]
    fn an_unavailable_beta_explains_the_stable_version_rule() {
        let note = availability_note_if(ZebradChannel::Beta, false).expect("Beta is unavailable");
        assert!(note.contains(ZebradChannel::Beta.version()), "{note}");
        assert!(note.contains(ZebradChannel::Stable.version()), "{note}");
        assert!(note.contains("not newer"), "{note}");
        assert_eq!(availability_note(ZebradChannel::Beta, true), Some(note));
        for channel in ZebradChannel::ALL {
            assert_eq!(availability_note_if(channel, true), None);
            assert_eq!(
                availability_note(channel, false).is_none(),
                channel.is_available()
            );
        }
    }

    #[test]
    fn prereleases_warn_about_experimental_databases_and_fixed_channels() {
        assert_eq!(channel_warning(ZebradChannel::Stable), None);
        for channel in [ZebradChannel::Beta, ZebradChannel::Nightly] {
            let warning = channel_warning(channel)
                .expect("a prerelease warning")
                .to_lowercase();
            assert!(warning.contains("experimental"), "{warning}");
            assert!(warning.contains("database"), "{warning}");
            assert!(warning.contains("stable cannot open"), "{warning}");
            assert!(warning.contains("cannot be changed"), "{warning}");
            let build = channel_build(channel);
            assert!(build.contains(channel.version()), "{build}");
            assert!(build.contains(channel.short_commit()), "{build}");
            assert!(build.contains(channel.date()), "{build}");
        }
    }

    #[test]
    fn every_selection_survives_in_the_draft_program() {
        assert_eq!(NodeProgram::default().channel(), ZebradChannel::Stable);
        for channel in ZebradChannel::ALL {
            assert_eq!(draft_program(channel).channel(), channel);
        }
    }
}
