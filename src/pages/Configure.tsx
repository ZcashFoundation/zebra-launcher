import { invoke } from "@tauri-apps/api/core";
import { createSignal, onMount } from "solid-js";
import { styled } from "solid-styled-components";

const PageContainer = styled("div")`
  display: flex;
  flex-grow: 1;
  flex-direction: column;
  padding: 0 24px 24px;
  font-family: sans-serif;
`;

const Configuration = () => {
  const [config_contents, set_config_contents] = createSignal<string>("");

  onMount(async () => {
    set_config_contents(await invoke("read_config"));
  });

  const save_and_apply = () => {
    invoke("save_config", { new_config: config_contents() });
  };

  return (
    <PageContainer>
      <h1>Configuration</h1>
      <textarea
        value={config_contents()}
        onChange={({ currentTarget: { value } }) => set_config_contents(value)}
      />
      <button onClick={save_and_apply}>Save & Apply</button>
    </PageContainer>
  );
};

export default Configuration;
