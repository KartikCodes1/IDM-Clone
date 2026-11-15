import { useId, useState, type FormEvent } from "react";

import { messageOf } from "../hooks/useEngine";
import { enqueueUrl } from "../lib/commands";
import { validateAddForm, type AddFormErrors } from "../lib/validate";

interface Props {
  /** Called with the file name the engine chose, after a successful add. */
  onAdded: (fileName: string) => void;
}

function Field(props: {
  label: string;
  value: string;
  error: string | undefined;
  placeholder: string;
  onChange: (value: string) => void;
  hint?: string;
}) {
  const id = useId();
  const errorId = `${id}-error`;
  return (
    <div className="field">
      <label htmlFor={id}>{props.label}</label>
      <input
        id={id}
        type="text"
        value={props.value}
        placeholder={props.placeholder}
        autoComplete="off"
        spellCheck={false}
        aria-invalid={props.error !== undefined}
        aria-describedby={props.error === undefined ? undefined : errorId}
        onChange={(event) => props.onChange(event.target.value)}
      />
      {props.error !== undefined && (
        <p id={errorId} className="field-error" role="alert">
          {props.error}
        </p>
      )}
    </div>
  );
}

/** URL, optional folder and optional SHA-256, validated here and again by the engine. */
export function AddDownload({ onAdded }: Props) {
  const [url, setUrl] = useState("");
  const [folder, setFolder] = useState("");
  const [sha256, setSha256] = useState("");
  const [errors, setErrors] = useState<AddFormErrors>({});
  const [submitError, setSubmitError] = useState<string>();
  const [submitting, setSubmitting] = useState(false);

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (submitting) return;
    setSubmitError(undefined);
    const result = validateAddForm({ url, folder, sha256 });
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setSubmitting(true);
    try {
      const added = await enqueueUrl({
        url: result.value.url,
        ...(result.value.folder === undefined ? {} : { targetDir: result.value.folder }),
        ...(result.value.sha256 === undefined ? {} : { sha256: result.value.sha256 }),
      });
      setUrl("");
      setSha256("");
      onAdded(added.file_name);
    } catch (err) {
      setSubmitError(messageOf(err));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <form className="add-panel" onSubmit={(event) => void submit(event)} noValidate aria-label="Add download">
      <div className="add-grid">
        <Field label="Download link" value={url} error={errors.url} placeholder="https://example.com/file.zip" onChange={setUrl} />
        <Field label="Save to folder (optional)" value={folder} error={errors.folder} placeholder="Default: your Downloads folder" onChange={setFolder} />
        <Field label="SHA-256 checksum (optional)" value={sha256} error={errors.sha256} placeholder="64 hexadecimal characters" onChange={setSha256} />
        <button type="submit" className="button add-submit" disabled={submitting}>
          Add
        </button>
      </div>
      {submitError !== undefined && (
        <p className="field-error" role="alert">
          {submitError}
        </p>
      )}
    </form>
  );
}
