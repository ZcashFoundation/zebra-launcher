import { Router, Route, RouteSectionProps, A, useMatch } from "@solidjs/router";
import { css, styled } from "solid-styled-components";

import { listen, Event, UnlistenFn } from "@tauri-apps/api/event";

import { createSignal, onCleanup, onMount } from "solid-js";

import { EXAMPLE_LOGS } from "./tests/example_data";
import { MAX_NUM_LOG_LINES } from "./constants";

import { NAVIGATION_BAR_HEIGHT } from "./constants";
import Logs from "./pages/Logs";
import Configuration from "./pages/Configure";

const TabNavigation = styled("header")`
  display: flex;
  position: fixed;
  font-size: 12px;
  background-color: #1c1c1c;
  margin: 0;
  height: ${NAVIGATION_BAR_HEIGHT};
  width: 100%;
  box-sizing: border-box;
  list-style-type: none;
  padding: 0 0 12px;
`;

const Container = styled("div")`
  display: flex;
  flex-grow: 1;
  flex-direction: column;
  overflow-x: hidden;
  overflow-y: hidden;
  background-color: #1c1c1c;
`;

const ClearFixedNavSpace = styled("div")`
  padding-top: ${NAVIGATION_BAR_HEIGHT};
`;

const NavItem = ({ children, href }: { children: string; href: string }) => {
  const is_active = useMatch(() => href);
  return (
    <A
      href={href}
      class={css`
        text-transform: uppercase;
        font-family: sans-serif;
        padding: 16px 4px 6px;
        margin: 0 20px;
        vertical-align: middle;
        text-decoration: none;
        border-bottom: solid 1px ${is_active() ? "#fff" : "transparent"};
        color: ${is_active() ? "#fff" : "#888"};

        &:hover {
          color: #fff;
        }
      `}
    >
      {children}
    </A>
  );
};

const AppContainer = ({ children }: RouteSectionProps) => (
  <Container>
    <TabNavigation>
      <NavItem href="/">Logs</NavItem>
      <NavItem href="/configure">Configure</NavItem>
    </TabNavigation>
    <ClearFixedNavSpace />
    {children}
  </Container>
);

function App() {
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

      // TODO: check if it's the logs page? May be easier to do if this logic is moved to `AppContainer`.
      if (was_at_bottom) {
        scroll_to_bottom();
      }

      example_log_index = (example_log_index + 1) % EXAMPLE_LOGS.length;
    }, 100);
  }

  return (
    <Router root={AppContainer}>
      <Route path="/" component={() => <Logs logs={logs} />} />
      <Route path="/configure" component={Configuration} />
    </Router>
  );
}

export default App;
