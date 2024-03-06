import { For, Accessor } from "solid-js";
import { styled } from "solid-styled-components";

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

const Logs = ({ logs }: { logs: Accessor<string[]> }) => {
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
