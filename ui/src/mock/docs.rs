//! Offline documentation for the browser mock. The paths mirror the packaged resources, while
//! [`srcdoc`] supplies small, representative documents without files, a server or network access.

use launcher_types::{NodeDocs, NodeId, NodeKind, ZebradChannel};

use crate::pages::docs::DocsSection;

/// The illustrative paths returned by the mock command, one set for the node's fixed channel.
pub fn for_channel(channel: ZebradChannel) -> NodeDocs {
    let root = format!("/mock/zebra-docs/{}", channel.as_str());
    NodeDocs {
        channel,
        book: format!("{root}/book/index.html"),
        config: format!("{root}/config.html"),
        changes: format!("{root}/changes.html"),
    }
}

pub async fn get_node_docs(node: &NodeId) -> Result<NodeDocs, String> {
    let info = super::nodes::list_nodes()
        .await
        .into_iter()
        .find(|info| info.id == *node)
        .ok_or_else(|| format!("there is no node {node}"))?;
    if info.kind != NodeKind::Zebrad {
        return Err("offline Zebra documentation is available only for Zebra nodes".into());
    }
    Ok(for_channel(info.program.channel()))
}

/// A self-contained document for the browser mock. Packaged documents receive their theme as a
/// query parameter; inline documents instead carry the resolved theme in their HTML and CSS.
pub fn srcdoc(channel: ZebradChannel, section: DocsSection, dark: bool) -> String {
    let theme = if dark { "dark" } else { "light" };
    let (background, foreground, muted, border, accent, panel) = if dark {
        (
            "#1c1c1c", "#eeeeee", "#b5b5b5", "#3a3a3a", "#f4b728", "#262626",
        )
    } else {
        (
            "#f6f6f4", "#1a1a1a", "#52514e", "#d9d9d4", "#7d5a06", "#ffffff",
        )
    };
    let label = channel.label();
    let version = channel.version();
    let commit = channel.short_commit();
    let date = channel.date();
    let notice = match channel {
        ZebradChannel::Stable => "Stable is the recommended Zebra release channel.",
        ZebradChannel::Beta => {
            "Beta preview: this release is still being tested. A node's channel cannot be changed after creation."
        }
        ZebradChannel::Nightly => {
            "Nightly preview: experimental development software. A node's channel cannot be changed after creation."
        }
    };
    let title = section.title();
    let body = match section {
        DocsSection::Book => format!(
            "<h1>Zebra {label} book</h1>\
             <p>This offline browser example represents the book bundled for this node's exact Zebra release.</p>\
             <h2>Running a node</h2>\
             <p>Zeal manages the <code>zebrad</code> process, its configuration and its chain state. Use the Dashboard to follow synchronization and the Logs page to inspect the process output.</p>\
             <h2>Networks</h2>\
             <p>Mainnet and Testnet nodes have separate configurations and chain databases. Testnet remains clearly marked throughout the launcher.</p>\
             <h2>Data safety</h2>\
             <p>Keep separate data directories for separate nodes. Zebra releases can upgrade the database format; use the channel chosen when the node was created.</p>"
        ),
        DocsSection::Config => format!(
            "<h1>Configuration reference</h1>\
             <p>Representative settings from the Zebra {label} configuration reference.</p>\
             <table><thead><tr><th>Setting</th><th>Purpose</th></tr></thead><tbody>\
             <tr><td><code>network.network</code></td><td>Selects <code>Mainnet</code> or <code>Testnet</code>.</td></tr>\
             <tr><td><code>rpc.listen_addr</code></td><td>Sets the JSON-RPC listener used by local tools.</td></tr>\
             <tr><td><code>metrics.endpoint_addr</code></td><td>Publishes Prometheus metrics for the Dashboard and Grafana.</td></tr>\
             <tr><td><code>state.cache_dir</code></td><td>Locates this node's chain state and database.</td></tr>\
             </tbody></table>\
             <p>The Configure page edits the node's actual <code>zebrad.toml</code>; this reference describes the bundled release offline.</p>"
        ),
        DocsSection::Changes => format!(
            "<h1>Zebra {version} release notes</h1>\
             <dl><dt>Channel</dt><dd>{label}</dd><dt>Commit</dt><dd><code>{commit}</code></dd><dt>Release date</dt><dd>{date}</dd></dl>\
             <h2>About this bundle</h2>\
             <p>These notes belong to the exact binary selected for this node. The documentation is bundled with Zeal and remains available without a network connection.</p>\
             <p>Review configuration and database changes before upgrading or restoring a chain snapshot.</p>"
        ),
    };
    format!(
        "<!doctype html><html lang=\"en\" data-theme=\"{theme}\"><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; script-src 'none'; connect-src 'none'; base-uri 'none'; form-action 'none'\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"{theme}\"><title>{title} · Zebra {label}</title>\
         <style>html{{color-scheme:{theme}}}body{{margin:0;padding:32px;background:{background};color:{foreground};font:16px/1.6 system-ui,sans-serif}}\
         main{{max-width:880px;margin:auto}}h1{{line-height:1.2}}h2{{margin-top:28px}}a{{color:{accent}}}\
         .identity{{padding:12px 16px;border:1px solid {border};border-radius:8px;background:{panel};color:{muted}}}\
         .identity strong{{color:{foreground}}}code{{font-family:ui-monospace,monospace}}\
         table{{width:100%;border-collapse:collapse}}th,td{{padding:10px;border:1px solid {border};text-align:left;vertical-align:top}}\
         dt{{float:left;clear:left;width:120px;color:{muted}}}dd{{margin:0 0 8px 130px}}</style></head>\
         <body><main><p class=\"identity\"><strong>Zebra {label} {version}</strong> · commit {commit} · {date}<br>{notice}</p>{body}</main></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_channel_has_its_own_three_document_paths() {
        for channel in ZebradChannel::ALL {
            let docs = for_channel(channel);
            assert_eq!(docs.channel, channel);
            for path in [&docs.book, &docs.config, &docs.changes] {
                assert!(path.starts_with(&format!("/mock/zebra-docs/{}/", channel.as_str())));
                assert!(path.ends_with(".html"));
            }
            assert!(docs.book.ends_with("/book/index.html"));
            assert!(docs.config.ends_with("/config.html"));
            assert!(docs.changes.ends_with("/changes.html"));
        }
    }

    #[test]
    fn inline_documents_cover_every_section_channel_and_theme() {
        for channel in ZebradChannel::ALL {
            for section in DocsSection::ALL {
                for dark in [false, true] {
                    let html = srcdoc(channel, section, dark);
                    assert!(html.contains(channel.label()));
                    assert!(html.contains(channel.version()));
                    assert!(html.contains(channel.short_commit()));
                    assert!(html.contains(channel.date()));
                    assert!(html.contains(section.title()));
                    assert!(html.contains(if dark {
                        "color-scheme:dark"
                    } else {
                        "color-scheme:light"
                    }));
                    assert!(html.contains(
                        "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; script-src 'none'; connect-src 'none'; base-uri 'none'; form-action 'none'\">"
                    ));
                    assert!(!html.contains("<script"));
                    assert!(!html.contains("http://"));
                    assert!(!html.contains("https://"));
                }
            }
        }
    }
}
