use futures::StreamExt;

use leptos::leptos_dom::ev::SubmitEvent;
use leptos::*;
use serde::{Deserialize, Serialize};
use tauri_sys::{event, tauri};

#[derive(Serialize, Deserialize)]
struct GreetArgs<'a> {
    name: &'a str,
}

async fn listen_for_logs(event_writer: WriteSignal<Vec<String>>) {
    let mut events = event::listen::<String>("log").await.unwrap();

    while let Some(event) = events.next().await {
        event_writer.update(|all_events| all_events.push(event.payload));
    }
}

#[component]
pub fn App() -> impl IntoView {
    let (name, set_name) = create_signal(String::new());
    let (greet_msg, set_greet_msg) = create_signal(String::new());

    let (logs, set_logs) = create_signal::<Vec<String>>(vec![]);
    create_local_resource(move || set_logs, listen_for_logs);

    let update_name = move |ev| {
        let v = event_target_value(&ev);
        set_name.set(v);
    };

    let greet = move |ev: SubmitEvent| {
        ev.prevent_default();
        spawn_local(async move {
            let name = &name.get_untracked();
            if name.is_empty() {
                return;
            }

            // Learn more about Tauri commands at https://tauri.app/v1/guides/features/command
            let new_msg = tauri::invoke::<_, String>("greet", &GreetArgs { name })
                .await
                .unwrap();
            set_greet_msg.set(new_msg);
        });
    };

    view! {
        <main class="container">
            <div class="row">
                <a href="https://tauri.app" target="_blank">
                    <img src="public/tauri.svg" class="logo tauri" alt="Tauri logo"/>
                </a>
                <a href="https://docs.rs/leptos/" target="_blank">
                    <img src="public/leptos.svg" class="logo leptos" alt="Leptos logo"/>
                </a>
            </div>

            <p>"Click on the Tauri and Leptos logos to learn more."</p>

            <p>
                "Recommended IDE setup: " <a href="https://code.visualstudio.com/" target="_blank">
                    "VS Code"
                </a> " + " <a href="https://github.com/tauri-apps/tauri-vscode" target="_blank">
                    "Tauri"
                </a> " + " <a href="https://github.com/rust-lang/rust-analyzer" target="_blank">
                    "rust-analyzer"
                </a>
            </p>

            <form class="row" on:submit=greet>
                <input id="greet-input" placeholder="Enter a name..." on:input=update_name/>
                <button type="submit">"Greet"</button>
            </form>

            <p>
                <b>{move || greet_msg.get()}</b>
            </p>

            <For
                // a function that returns the items we're iterating over; a signal is fine
                each=move || logs.get()
                // a unique key for each item
                key=|log| log.clone()
                // renders each item to a view
                children=move |log: String| {
                    view! { <code>{log}</code> }
                }
            />

        </main>
    }
}
