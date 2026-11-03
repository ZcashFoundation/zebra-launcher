//! The firewall step: the commands that open the front door's port, for the tool on `PATH`.
//! Zeal shows them and never runs them.

use launcher_types::public::{FirewallInfo, FirewallTool};

/// The commands for `tool` and the front door's TCP `port`, to allow it and to remove the rule
/// again. Without a tool, nothing to show: Ubuntu ships ufw disabled and Fedora Workstation
/// opens 1025 to 65535, and the outside checks prove the rest.
pub fn firewall_info(tool: Option<FirewallTool>, port: u16) -> FirewallInfo {
    let (allow, remove) = match tool {
        Some(FirewallTool::Ufw) => (
            vec![format!("sudo ufw allow {port}/tcp")],
            vec![format!("sudo ufw delete allow {port}/tcp")],
        ),
        Some(FirewallTool::Firewalld) => (
            vec![format!(
                "sudo firewall-cmd --permanent --add-port={port}/tcp && sudo firewall-cmd --reload"
            )],
            vec![format!(
                "sudo firewall-cmd --permanent --remove-port={port}/tcp && sudo firewall-cmd --reload"
            )],
        ),
        None => (Vec::new(), Vec::new()),
    };
    FirewallInfo {
        tool,
        allow,
        remove,
    }
}

/// The tool to show commands for, from the programs found on `PATH`: firewalld's
/// `firewall-cmd` first (its rules replace ufw's where both exist), then `ufw`.
pub fn pick_tool(has_firewall_cmd: bool, has_ufw: bool) -> Option<FirewallTool> {
    if has_firewall_cmd {
        Some(FirewallTool::Firewalld)
    } else if has_ufw {
        Some(FirewallTool::Ufw)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_name_the_port_and_undo_themselves() {
        let ufw = firewall_info(Some(FirewallTool::Ufw), 8443);
        assert_eq!(ufw.allow, ["sudo ufw allow 8443/tcp"]);
        assert_eq!(ufw.remove, ["sudo ufw delete allow 8443/tcp"]);
        let firewalld = firewall_info(Some(FirewallTool::Firewalld), 8443);
        assert_eq!(
            firewalld.allow,
            ["sudo firewall-cmd --permanent --add-port=8443/tcp && sudo firewall-cmd --reload"]
        );
        assert!(firewalld.remove[0].contains("--remove-port=8443/tcp"));
        assert!(firewall_info(None, 8443).allow.is_empty());
        assert_eq!(pick_tool(true, true), Some(FirewallTool::Firewalld));
        assert_eq!(pick_tool(false, true), Some(FirewallTool::Ufw));
        assert_eq!(pick_tool(false, false), None);
    }
}
