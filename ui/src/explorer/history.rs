//! Back and forward in one node's Explorer. Not the webview's history: tabs and nodes are not
//! browser history, and no route reaches the webview's URL or storage. Pure, tested natively.

use launcher_types::explorer::ExplorerRoute;

/// The most routes kept; the oldest go first.
pub const MAX_ENTRIES: usize = 100;

/// The routes visited, and where in them the page is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavHistory {
    entries: Vec<ExplorerRoute>,
    at: usize,
}

impl Default for NavHistory {
    /// The Chain page, with nowhere to go back to.
    fn default() -> Self {
        Self {
            entries: vec![ExplorerRoute::Chain],
            at: 0,
        }
    }
}

impl NavHistory {
    /// The route shown.
    pub fn current(&self) -> &ExplorerRoute {
        &self.entries[self.at]
    }

    /// Goes to `route`, dropping the routes ahead. Going where the page already is changes
    /// nothing.
    pub fn push(&mut self, route: ExplorerRoute) {
        if *self.current() == route {
            return;
        }
        self.entries.truncate(self.at + 1);
        self.entries.push(route);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
        self.at = self.entries.len() - 1;
    }

    /// Shows `route` in place of the current one, so going back skips the replaced route: a
    /// search that resolved to its page.
    pub fn replace(&mut self, route: ExplorerRoute) {
        self.entries[self.at] = route;
    }

    pub fn can_go_back(&self) -> bool {
        self.at > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.at + 1 < self.entries.len()
    }

    /// One step back, if there is one.
    pub fn back(&mut self) -> Option<&ExplorerRoute> {
        self.can_go_back().then(|| {
            self.at -= 1;
            &self.entries[self.at]
        })
    }

    /// One step forward, if there is one.
    pub fn forward(&mut self) -> Option<&ExplorerRoute> {
        self.can_go_forward().then(|| {
            self.at += 1;
            &self.entries[self.at]
        })
    }
}

#[cfg(test)]
mod tests {
    use launcher_types::explorer::BlockId;

    use super::*;

    fn block(height: u32) -> ExplorerRoute {
        ExplorerRoute::Block {
            id: BlockId::Height(height),
        }
    }

    #[test]
    fn back_forward_and_a_new_branch() {
        let mut history = NavHistory::default();
        assert!(!history.can_go_back());
        history.push(block(1));
        history.push(block(2));
        assert_eq!(history.back(), Some(&block(1)));
        assert_eq!(history.back(), Some(&ExplorerRoute::Chain));
        assert_eq!(history.back(), None);
        assert_eq!(history.forward(), Some(&block(1)));
        // A new route drops the ones ahead.
        history.push(block(3));
        assert!(!history.can_go_forward());
        assert_eq!(history.back(), Some(&block(1)));
        // The same route twice is one entry.
        history.forward();
        history.push(block(3));
        assert_eq!(history.back(), Some(&block(1)));
    }

    #[test]
    fn a_replaced_route_is_skipped_on_the_way_back() {
        let mut history = NavHistory::default();
        history.push(ExplorerRoute::Search { query: "ab".into() });
        history.replace(block(7));
        assert_eq!(history.current(), &block(7));
        assert_eq!(history.back(), Some(&ExplorerRoute::Chain));
    }

    #[test]
    fn the_oldest_routes_go_first() {
        let mut history = NavHistory::default();
        for height in 0..150 {
            history.push(block(height));
        }
        let mut steps = 0;
        while history.back().is_some() {
            steps += 1;
        }
        assert_eq!(steps, MAX_ENTRIES - 1);
        assert_eq!(history.current(), &block(50));
    }
}
