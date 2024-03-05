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

const ConfigTextArea = styled("textarea")``;

const ConfigDisplay = ({ children }: { children: string }) => {
  return <code>{children}</code>;
};

const Configuration = () => {
  const [config_contents, set_config_contents] = createSignal<string>("");
  const [edited_config, set_edited_config] = createSignal<string | null>(null);

  onMount(async () => {
    set_config_contents(await invoke("read_config"));
  });

  const save_and_apply = () => {
    invoke("save_config", { new_config: edited_config() });
  };

  const discard_changes = () => {
    set_edited_config(null);
  };

  const start_editing = () => {
    set_edited_config("");
  };

  const is_editable = () => edited_config() !== null;

  return (
    <PageContainer>
      <h1>Configuration</h1>

      {is_editable() ? (
        <>
          <ConfigTextArea
            value={edited_config() || ""}
            onChange={({ currentTarget: { value } }) =>
              set_edited_config(value)
            }
          />{" "}
          <button onClick={discard_changes}>Discard Changes</button>
          <button onClick={save_and_apply}>Save & Apply</button>
        </>
      ) : (
        <>
          <ConfigDisplay>{config_contents()}</ConfigDisplay>
          <button onClick={start_editing}>Edit</button>
        </>
      )}
    </PageContainer>
  );
};

export default Configuration;
