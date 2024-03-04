// Modified from <https://github.com/fabian-hiller/modular-forms/blob/main/playgrounds/solid/src/components/Checkbox.tsx>

import { JSX, splitProps } from "solid-js";
import { css } from "solid-styled-components";

type CheckboxProps = {
  ref: (element: HTMLInputElement) => void;
  name: string;
  value?: string;
  checked?: boolean;
  onInput: JSX.EventHandler<HTMLInputElement, InputEvent>;
  onChange: JSX.EventHandler<HTMLInputElement, Event>;
  onBlur: JSX.EventHandler<HTMLInputElement, FocusEvent>;
  required?: boolean;
  class?: string;
  label: string;
  error?: string;
  padding?: "none";
};

/**
 * Checkbox that allows users to select an option. The label next to the
 * checkbox describes the selection option.
 */
export function Checkbox(props: CheckboxProps) {
  const [, inputProps] = splitProps(props, [
    "class",
    "value",
    "label",
    "error",
    "padding",
  ]);
  return (
    <div>
      <label
        class={css`
          border: solid 1px ${props.checked ? "#fff" : "#888"};
          border-radius: 8px;
          display: flex;
        `}
      >
        <span
          class={css`
                padding 8px 16px;
                display: flex;
                flex-grow: 1;
                color: ${props.checked ? "#fff" : "#888"};
            `}
        >
          {props.label}
        </span>
        <input
          {...inputProps}
          class={css`
            margin: auto 8px;
            width: 24px;
          `}
          type="checkbox"
          id={props.name}
          value={props.value || ""}
          checked={props.checked}
          aria-invalid={!!props.error}
          aria-errormessage={`${props.name}-error`}
        />
      </label>
    </div>
  );
}
