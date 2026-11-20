// A question card an agent showed with `ask_user`: one question at a time,
// answers sent together as one message. Locked once answered. A daemon card's
// secret field sits on the last step and is never shown again.
import type { JSX } from "preact";
import { useState } from "preact/hooks";
import { Check, ChevronLeft, CircleHelp } from "lucide-preact";

import type { Agent, QuestionAnswer, QuestionAnswered, QuestionCard as Card } from "../api.ts";
import { useApp } from "../context.ts";
import { Button } from "./ui.tsx";

type Question = Card["questions"][number];
/** A question's answer while the user edits it. */
interface Draft {
  values: string[];
  custom: string;
}
const EMPTY: Draft = { values: [], custom: "" };

/** True when the draft answer is complete for this question. */
export function isAnswered(q: Question, a: Draft | undefined): boolean {
  const custom = (a?.custom ?? "").trim() !== "";
  const picked = a?.values.length ?? 0;
  if (q.kind === "text") return custom;
  if (q.kind === "single") return picked + (custom ? 1 : 0) === 1;
  return picked > 0 || custom;
}

/** Human-readable answer, e.g. "Email, Chat". */
export function answerLabel(q: Question, a: QuestionAnswer | undefined): string {
  if (!a) return "";
  const parts = (a.values ?? []).map(
    (v) => (q.choices ?? []).find((c) => c.value === v)?.label ?? v,
  );
  const custom = (a.custom ?? "").trim();
  if (custom) parts.push(custom);
  return parts.join(", ");
}

export function QuestionCardView(props: {
  agent: Agent;
  id: number;
  card: Card;
  answered: QuestionAnswered | null;
}): JSX.Element {
  const { card, answered, agent } = props;
  const { client, run } = useApp();
  const [step, setStep] = useState(0);
  const [draft, setDraft] = useState<Map<number, Draft>>(new Map());
  const [sending, setSending] = useState(false);
  const [secretValue, setSecretValue] = useState("");
  const secret = card.questions.find((x) => x.kind === "secret");
  const steps = card.questions.filter((x) => x.kind !== "secret");
  const total = steps.length;

  const head = (
    <div class="qcard-head">
      <span class="qcard-title">
        {card.title ?? (total === 1 ? "A question for you" : "A few questions for you")}
      </span>
      <span class="spacer" />
      {answered ? (
        <span class="qcard-done">
          <Check size={16} /> Answered
        </span>
      ) : (
        total > 1 && (
          <span class="qcard-step">
            {step + 1} of {total}
          </span>
        )
      )}
    </div>
  );

  if (answered) {
    return (
      <div class="row row-tool">
        <div class="qcard locked">
          {head}
          <dl class="qcard-summary">
            {steps.map((q) => (
              <div key={q.id}>
                <dt>{q.prompt}</dt>
                <dd>
                  {answerLabel(
                    q,
                    answered.answers.find((a) => a.question_id === q.id),
                  )}
                </dd>
              </div>
            ))}
            {secret && answered.answers[0]?.values?.includes("approve") && (
              <div>
                <dt>{secret.prompt}</dt>
                <dd>Saved in Vault</dd>
              </div>
            )}
          </dl>
        </div>
      </div>
    );
  }

  const q = steps[step] as Question;
  const choices = q.choices ?? [];
  const cur = draft.get(step) ?? EMPTY;
  const update = (next: Draft): void => {
    setDraft(new Map(draft).set(step, next));
  };
  const toggle = (value: string): void => {
    if (q.kind === "single") update({ values: cur.values[0] === value ? [] : [value], custom: "" });
    else
      update({
        ...cur,
        values: cur.values.includes(value)
          ? cur.values.filter((v) => v !== value)
          : [...cur.values, value],
      });
  };
  const setCustom = (text: string): void => {
    update({ ...cur, custom: text, values: q.kind === "single" && text.trim() ? [] : cur.values });
  };
  const last = step === total - 1;
  const approving = (draft.get(0) ?? EMPTY).values.includes("approve");
  const ready = isAnswered(q, cur) && !(last && secret && approving && !secretValue.trim());
  const submit = async (): Promise<void> => {
    setSending(true);
    const answers: QuestionAnswer[] = card.questions.map((x, i) => {
      if (x === secret)
        return { question_id: x.id ?? "", values: [], custom: secretValue.trim() || null };
      const a = draft.get(steps.indexOf(x)) ?? EMPTY;
      return {
        question_id: x.id ?? `q${String(i + 1)}`,
        values: a.values,
        custom: a.custom.trim() || null,
      };
    });
    await run(() => client.answerQuestion(agent.id, props.id, answers));
    setSending(false);
  };

  return (
    <div class="row row-tool">
      <form
        class="qcard"
        onSubmit={(e) => {
          e.preventDefault();
          if (!ready) return;
          if (last) void submit();
          else setStep(step + 1);
        }}
      >
        {head}
        <p class="qcard-prompt">
          <CircleHelp size={16} /> {q.prompt}
        </p>
        {q.kind !== "text" && (
          <div class="qcard-choices" role={q.kind === "single" ? "radiogroup" : "group"}>
            {choices.map((c) => {
              const value = c.value ?? c.label;
              const on = cur.values.includes(value);
              return (
                <button
                  key={value}
                  type="button"
                  class={`choice${on ? " on" : ""}${q.kind === "multi" ? " multi" : ""}`}
                  role={q.kind === "single" ? "radio" : "checkbox"}
                  aria-checked={on}
                  onClick={() => {
                    toggle(value);
                  }}
                >
                  <span class="choice-mark">{on && <Check size={12} />}</span>
                  {c.label}
                </button>
              );
            })}
          </div>
        )}
        {(q.kind === "text" || q.allow_custom) && (
          <input
            class="qcard-custom"
            value={cur.custom}
            placeholder={q.kind === "text" ? "Your answer" : "Something else…"}
            onInput={(e) => {
              setCustom(e.currentTarget.value);
            }}
          />
        )}
        {last && secret && (
          <label class="qcard-secret">
            {secret.prompt}
            <input
              class="qcard-custom"
              type="password"
              autoComplete="off"
              spellcheck={false}
              value={secretValue}
              onInput={(e) => {
                setSecretValue(e.currentTarget.value);
              }}
            />
            <span class="hint">Goes straight to Vault. The agent never sees it.</span>
          </label>
        )}
        <div class="qcard-foot">
          {q.kind === "multi" && <span class="muted small">Pick any</span>}
          <span class="spacer" />
          {step > 0 && (
            <Button
              variant="ghost"
              small
              onClick={() => {
                setStep(step - 1);
              }}
            >
              <ChevronLeft size={16} /> Back
            </Button>
          )}
          <Button variant="primary" small type="submit" disabled={!ready || sending}>
            {last ? "Send answers" : "Next"}
          </Button>
        </div>
      </form>
    </div>
  );
}
