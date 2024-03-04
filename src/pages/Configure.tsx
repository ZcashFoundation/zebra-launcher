import { createForm } from "@modular-forms/solid";
import { css, styled } from "solid-styled-components";
import { Checkbox } from "../components/checkbox";

const PageContainer = styled("div")`
  display: flex;
  flex-grow: 1;
  flex-direction: column;
  padding: 0 24px 24px;
  font-family: sans-serif;
`;

type ConsensusConfig = {
  checkpoint_sync: boolean;
};

// Note: Interfaces don't seem to work well with @modular-forms
type ZebradConfig = {
  consensus: ConsensusConfig;
};

const Configuration = () => {
  // TODO: Read in this initial value from Zebra's existing config file
  //       (Generate the default config if none exists)
  const [, { Form, Field }] = createForm<ZebradConfig>({
    initialValues: {
      consensus: { checkpoint_sync: true },
    },
  });

  const save_and_apply = (values: ZebradConfig) => {
    console.log(values);
  };

  return (
    <PageContainer>
      <h1>Configuration</h1>
      <div style={{ "max-width": "600px" }}>
        <Form onSubmit={save_and_apply}>
          <Field name="consensus.checkpoint_sync" type="boolean">
            {(field, props) => (
              <Checkbox
                {...props}
                checked={field.value}
                error={field.error}
                label="Enable Checkpoint Sync"
              />
            )}
          </Field>

          <div>
            <button
              type="submit"
              class={css`
                display: inline-block;
                margin: 16px 0 0;
                border-radius: 4px;
                padding: 8px 16px;
                background: transparent;
                color: white;
                border: solid 1px white;
                cursor: pointer;
              `}
            >
              Save & Apply
            </button>
          </div>
        </Form>
      </div>
    </PageContainer>
  );
};

export default Configuration;
