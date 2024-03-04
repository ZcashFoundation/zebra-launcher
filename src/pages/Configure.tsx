import { css, styled } from "solid-styled-components";
import { createForm, getValue } from "@modular-forms/solid";
import { Checkbox } from "../components/checkbox";

const PageContainer = styled("div")`
  display: flex;
  flex-grow: 1;
  flex-direction: column;
  padding: 0 24px 24px;
  font-family: sans-serif;
`;

const SubmitButton = styled("button")`
  display: inline-block;
  margin: 16px 0 0;
  border-radius: 4px;
  padding: 8px 16px;
  background: transparent;
  color: white;
  border: solid 1px white;
  cursor: pointer;
`;

type ConsensusConfig = {
  checkpoint_sync: boolean;
};

type NetworkConfig = {
  listen_addr: string;
  network: string;
  initial_mainnet_peers: string;
  initial_testnet_peers: string;
  cache_dir: { enabled: boolean; custom_path?: string };
  peerset_initial_target_size: number;
  crawl_new_peer_interval: number;
  max_connections_per_ip: number;
};

// Note: Interfaces don't seem to work well with @modular-forms
type ZebradConfig = {
  consensus: ConsensusConfig;
  network: NetworkConfig;
};

const DEFAULT_INITIAL_VALUES = {
  consensus: { checkpoint_sync: true },
  network: {
    listen_addr: "0.0.0.0:8233",
    network: "Mainnet",
    initial_mainnet_peers: `"dnsseed.z.cash:8233","dnsseed.str4d.xyz:8233","mainnet.seeder.zfnd.org:8233","mainnet.is.yolo.money:8233",`,
    initial_testnet_peers: `"dnsseed.testnet.z.cash:18233","testnet.seeder.zfnd.org:18233","testnet.is.yolo.money:18233"`,
    cache_dir: {
      enabled: true,
    },
    peerset_initial_target_size: 25,
    crawl_new_peer_interval: 61000,
    max_connections_per_ip: 1,
  },
};

const TextFieldLabel = styled("label")`
  border: solid 1px #fff;
  border-radius: 8px;
  display: flex;
  margin: 16px 0 0;
`;

const TextFieldLabelSpan = styled("span")`
  display: flex;
  padding: 8px 16px;
  color: #fff;
`;

const TextFieldInput = styled("input")`
  padding: 8px 16px;
  border: none;
  border-radius: 8px;
  background: none;
  color: #fff;
  display: flex;
  text-align: right;
  flex-grow: 1;

  &:focus-visible {
    outline: none;
  }
`;

const Configuration = () => {
  // TODO: Read in this initial value from Zebra's existing config file
  //       (Generate the default config if none exists)
  const [config_store, { Form, Field }] = createForm<ZebradConfig>({
    initialValues: DEFAULT_INITIAL_VALUES,
  });

  const save_and_apply = (values: ZebradConfig) => {
    console.log("save and apply");
    console.log(values);
  };

  return (
    <PageContainer>
      <h1>Configuration</h1>
      <div>
        <Form onSubmit={save_and_apply}>
          <h5>Consensus:</h5>

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

          <h5>Network:</h5>

          <Field name="network.listen_addr" type="string">
            {(field, props) => (
              <div>
                <TextFieldLabel>
                  <TextFieldLabelSpan class={css``}>
                    Listen Address:
                  </TextFieldLabelSpan>
                  <TextFieldInput
                    {...props}
                    type="text"
                    value={field.value}
                    class={css``}
                  />
                </TextFieldLabel>
              </div>
            )}
          </Field>

          <Field name="network.network" type="string">
            {(field, props) => (
              <div>
                <TextFieldLabel>
                  <TextFieldLabelSpan class={css``}>
                    Network Type (Mainnet/Testnet):
                  </TextFieldLabelSpan>
                  <TextFieldInput
                    {...props}
                    type="text"
                    value={field.value}
                    class={css``}
                  />
                </TextFieldLabel>
              </div>
            )}
          </Field>

          <Field name="network.initial_mainnet_peers" type="string">
            {(field, props) => (
              <div>
                <TextFieldLabel>
                  <TextFieldLabelSpan class={css``}>
                    Initial Mainnet Peers:
                  </TextFieldLabelSpan>
                  <TextFieldInput
                    {...props}
                    type="text"
                    value={field.value}
                    class={css``}
                  />
                </TextFieldLabel>
              </div>
            )}
          </Field>

          <Field name="network.initial_testnet_peers" type="string">
            {(field, props) => (
              <div>
                <TextFieldLabel>
                  <TextFieldLabelSpan class={css``}>
                    Initial Testnet Peers:
                  </TextFieldLabelSpan>
                  <TextFieldInput
                    {...props}
                    type="text"
                    value={field.value}
                    class={css``}
                  />
                </TextFieldLabel>
              </div>
            )}
          </Field>

          <Field name="network.cache_dir.enabled" type="boolean">
            {(field, props) => (
              <Checkbox
                {...props}
                checked={field.value}
                error={field.error}
                label="Enable Initial Peer Caching"
              />
            )}
          </Field>

          {getValue(config_store, "network.cache_dir.enabled") ? (
            <Field name="network.cache_dir.custom_path" type="string">
              {(field, props) => (
                <div>
                  <TextFieldLabel>
                    <TextFieldLabelSpan class={css``}>
                      Custom Initial Peer Cache Dir (optional):
                    </TextFieldLabelSpan>
                    <TextFieldInput
                      {...props}
                      type="text"
                      value={field.value}
                      class={css``}
                    />
                  </TextFieldLabel>
                </div>
              )}
            </Field>
          ) : null}

          <SubmitButton type="submit">Save & Apply</SubmitButton>
        </Form>
      </div>
    </PageContainer>
  );
};

export default Configuration;
