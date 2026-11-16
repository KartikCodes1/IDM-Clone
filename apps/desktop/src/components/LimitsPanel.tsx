import { useEffect, useId, useRef, useState, type FormEvent } from "react";

import { messageOf } from "../hooks/useEngine";
import { getLimits, updateGlobalLimits } from "../lib/commands";
import {
  describeLimits,
  formFromEffective,
  validateLimitsForm,
  type EffectiveLimits,
  type LimitsErrors,
  type LimitsForm,
  type SpeedUnit,
} from "../lib/limits";

const EMPTY_FORM: LimitsForm = { maxActive: "", speedUnit: "off", speedValue: "", connections: "" };

function isSpeedUnit(value: string): value is SpeedUnit {
  return value === "off" || value === "KB/s" || value === "MB/s";
}

function FieldError({ text }: { text: string | undefined }) {
  if (text === undefined) return null;
  return (
    <p className="field-error" role="alert">
      {text}
    </p>
  );
}

function NumberField(props: {
  label: string;
  value: string;
  error: string | undefined;
  onChange: (value: string) => void;
}) {
  const id = useId();
  return (
    <div className="field">
      <label htmlFor={id}>{props.label}</label>
      <input
        id={id}
        type="number"
        inputMode="numeric"
        value={props.value}
        aria-invalid={props.error !== undefined}
        onChange={(event) => props.onChange(event.target.value)}
      />
      <FieldError text={props.error} />
    </div>
  );
}

function SpeedField(props: { form: LimitsForm; error: string | undefined; onChange: (patch: Partial<LimitsForm>) => void }) {
  const valueId = useId();
  const off = props.form.speedUnit === "off";
  return (
    <div className="field">
      <label htmlFor={valueId}>Speed limit</label>
      <div className="speed-inputs">
        <input
          id={valueId}
          aria-label="Speed limit value"
          type="number"
          inputMode="decimal"
          value={props.form.speedValue}
          disabled={off}
          aria-invalid={props.error !== undefined}
          onChange={(event) => props.onChange({ speedValue: event.target.value })}
        />
        <select
          aria-label="Speed limit unit"
          value={props.form.speedUnit}
          onChange={(event) => {
            const unit = event.target.value;
            if (isSpeedUnit(unit)) props.onChange({ speedUnit: unit });
          }}
        >
          <option value="off">Off</option>
          <option value="KB/s">KB/s</option>
          <option value="MB/s">MB/s</option>
        </select>
      </div>
      <FieldError text={props.error} />
    </div>
  );
}

/** Global limits: loads what the engine reports, validates, applies, shows the effective values. */
export function LimitsPanel() {
  const [form, setForm] = useState<LimitsForm>(EMPTY_FORM);
  const [errors, setErrors] = useState<LimitsErrors>({});
  const [effective, setEffective] = useState<EffectiveLimits>();
  const [problem, setProblem] = useState<string>();
  const [applying, setApplying] = useState(false);
  const dirty = useRef(false);

  useEffect(() => {
    let cancelled = false;
    getLimits().then(
      (limits) => {
        if (cancelled) return;
        setEffective(limits);
        if (!dirty.current) setForm(formFromEffective(limits));
      },
      (err: unknown) => {
        if (!cancelled) setProblem(messageOf(err));
      },
    );
    return () => {
      cancelled = true;
    };
  }, []);

  async function apply(event: FormEvent) {
    event.preventDefault();
    if (applying) return;
    setProblem(undefined);
    const result = validateLimitsForm(form);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setApplying(true);
    try {
      const limits = await updateGlobalLimits(result.value);
      setEffective(limits);
      setForm(formFromEffective(limits));
    } catch (err) {
      setProblem(messageOf(err));
    } finally {
      setApplying(false);
    }
  }

  const patch = (change: Partial<LimitsForm>) => {
    dirty.current = true;
    setForm((current) => ({ ...current, ...change }));
  };
  return (
    <form id="limits-panel" className="limits-panel" onSubmit={(event) => void apply(event)} noValidate aria-label="Limits">
      <div className="limits-grid">
        <NumberField label="Active downloads (1-10)" value={form.maxActive} error={errors.maxActive} onChange={(v) => patch({ maxActive: v })} />
        <SpeedField form={form} error={errors.speed} onChange={patch} />
        <NumberField label="Connections per download (1-16)" value={form.connections} error={errors.connections} onChange={(v) => patch({ connections: v })} />
        <button type="submit" className="button limits-apply" disabled={applying || effective === undefined}>
          Apply limits
        </button>
      </div>
      {effective !== undefined && <p className="muted limits-effective">{describeLimits(effective)}</p>}
      <FieldError text={problem} />
    </form>
  );
}
