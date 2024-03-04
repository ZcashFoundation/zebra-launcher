import { listen, Event, UnlistenFn } from "@tauri-apps/api/event";

import { createSignal, onCleanup, onMount, For } from "solid-js";
import { styled } from "solid-styled-components";

import { EXAMPLE_LOGS } from "../tests/example_logs";
import { MAX_NUM_LOG_LINES } from "../constants";

const LogContainer = styled("div")`
  display: flex;
  flex-grow: 1;
  flex-direction: column;
  justify-content: end;
  padding: 4px;
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

const Log = ({ children }: { children: string }) => {
  const is_likely_timestamp = /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}.\d{6}Z/.test(
    children.slice(0, 27)
  );

  if (is_likely_timestamp) {
    return (
      <LogLine>
        <LogTimestamp>{children.slice(0, 27)}</LogTimestamp>
        <LogLevel>{children.slice(27, 34)}</LogLevel>
        <LogMessage>{children.slice(34)}</LogMessage>
      </LogLine>
    );
  } else {
    return (
      <LogLine>
        <LogMessage>{children}</LogMessage>
      </LogLine>
    );
  }
};

const Logs = () => {
  const is_tauri_app = window.hasOwnProperty("__TAURI_INTERNALS__");
  const [logs, set_logs] = createSignal<Array<string>>([]);

  const is_at_bottom = () => {
    const y_bottom = Math.ceil(window.scrollY) + window.innerHeight;
    return y_bottom >= document.body.scrollHeight;
  };

  const scroll_to_bottom = () => {
    window.scroll(0, document.body.scrollHeight);
  };

  if (is_tauri_app) {
    let stop_listening: UnlistenFn;

    onMount(async () => {
      stop_listening = await listen("log", (event: Event<string>) => {
        const was_at_bottom = is_at_bottom();

        set_logs([...logs().slice(-MAX_NUM_LOG_LINES), event.payload]);

        if (was_at_bottom) {
          scroll_to_bottom();
        }
      });
    });

    onCleanup(() => stop_listening());
  } else {
    let example_log_index = 0;
    setInterval(() => {
      const was_at_bottom = is_at_bottom();

      set_logs([
        ...logs().slice(-MAX_NUM_LOG_LINES),
        EXAMPLE_LOGS[example_log_index],
      ]);

      if (was_at_bottom) {
        scroll_to_bottom();
      }

      example_log_index = (example_log_index + 1) % EXAMPLE_LOGS.length;
    }, 100);
  }

  return (
    <LogContainer>
      <For each={logs()} fallback={<Log>Waiting for zebrad to start...</Log>}>
        {(log) => <Log>{log}</Log>}
      </For>
      <LogLine>&gt; _</LogLine>
    </LogContainer>
  );
};

export default Logs;
