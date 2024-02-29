import { createSignal, onCleanup, onMount } from "solid-js";
import { listen, Event, UnlistenFn } from "@tauri-apps/api/event";

import { styled } from "solid-styled-components";

const LogContainer = styled("div")`
  display: flex;
  flex-grow: 1;
  flex-direction: column;
  justify-content: end;
  overflow-x: hidden;
  overflow-y: scroll;
  background-color: #1c1c1c;
  padding: 8px;
`;

const LogLine = styled("div")`
  display: block;
  font-size: 12px;
  margin: 0;
  padding: 2px 4px;
`;

const LogTimestamp = styled("code")`
  display: inline;
  margin: 0;
  color: #888;
`;

const LogLevel = styled("code")`
  display: inline;
  margin: 0;
  color: ${({ children }: { children: string }) =>
    ({
      INFO: "green",
      WARN: "orange",
      TRACE: "blue",
      DEBUG: "purple",
    }[children.trim()] || "white")}};
  padding-left: 6px;
`;

const LogMessage = styled("code")`
  display: inline;
  margin: 0;
  color: #eee;
`;

const Log = ({ children }: { children: string }) => (
  <LogLine>
    <LogTimestamp>{children.slice(0, 27)}</LogTimestamp>
    <LogLevel>{children.slice(27, 34)}</LogLevel>
    <LogMessage>{children.slice(34)}</LogMessage>
  </LogLine>
);

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
    <>
      <LogContainer>
        {logs().map((log) => (
          <Log>{log}</Log>
        ))}
        <LogLine>&gt; _</LogLine>
      </LogContainer>
    </>
  );
}

export default App;
