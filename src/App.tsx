import { createSignal, onCleanup, onMount } from "solid-js";
import { listen, Event, UnlistenFn } from "@tauri-apps/api/event";
import "./App.css";

function App() {
  let stop_listening: UnlistenFn;

  const [logs, set_logs] = createSignal<Array<string>>([]);

  onMount(async () => {
    stop_listening = await listen("log", (event: Event<string>) =>
      set_logs([...logs(), event.payload])
    );
  });

  onCleanup(() => stop_listening());

  return (
    <div class="container">
      {logs().map((log) => (
        <code>{log}</code>
      ))}
    </div>
  );
}

export default App;
