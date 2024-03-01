import { Router, Route, RouteSectionProps } from "@solidjs/router";
import { styled } from "solid-styled-components";

import { NAVIGATION_BAR_HEIGHT } from "./constants";
import Logs from "./Logs";

const TabNavigation = styled("ul")`
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

const TabNavigationItem = styled("li")`
  text-transform: uppercase;
  font-family: sans-serif;
  padding: 16px 4px 6px;
  margin: 0 20px;
  vertical-align: middle;
  border-bottom: solid 1px #fff;
`;

const Container = styled("div")`
  display: flex;
  flex-grow: 1;
  flex-direction: column;
  overflow-x: hidden;
  overflow-y: hidden;
  background-color: #1c1c1c;
`;

const AppContainer = ({ children }: RouteSectionProps) => (
  <Container>
    <TabNavigation>
      <TabNavigationItem>Logs</TabNavigationItem>
    </TabNavigation>
    {children}
  </Container>
);

function App() {
  return (
    <Router root={AppContainer}>
      <Route path="/" component={Logs} />
    </Router>
  );
}

export default App;
