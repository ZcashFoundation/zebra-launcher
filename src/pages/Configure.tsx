import { styled } from "solid-styled-components";

const PageContainer = styled("div")`
  display: flex;
  flex-grow: 1;
  flex-direction: column;
  padding: 0 24px 24px;
  font-family: sans-serif;
`;

const Configuration = () => {
  // const save_and_apply = () => {};

  return (
    <PageContainer>
      <h1>Configuration</h1>
    </PageContainer>
  );
};

export default Configuration;
