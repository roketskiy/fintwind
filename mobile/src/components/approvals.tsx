import * as React from "react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
  FieldLegend,
  FieldSet,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { Textarea } from "@/components/ui/textarea";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { textBytes } from "@/lib/remote-validation";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import type { Permission, Question, Questions } from "@/lib/types";

const MAX_ANSWERS = 32;
const MAX_ANSWER_LENGTH = 4096;
const MAX_ANSWER_BYTES = 4096;

interface AnswerState {
  selected: string[];
  custom: string;
}

const emptyAnswer: AnswerState = { selected: [], custom: "" };

function optionValue(index: number) {
  return `option-${index}`;
}

function optionText(option: Question["options"][number], index: number) {
  return option.label || option.description || `选项 ${index + 1}`;
}

function questionKey(questions: Questions) {
  return JSON.stringify(questions);
}

function controlId(formId: string, questionIndex: number, suffix: string) {
  return `${formId}-q${questionIndex}-${suffix}`;
}

function firstControlId(
  formId: string,
  questionIndex: number,
  question: Question,
) {
  if (question.options.length > 0) {
    return controlId(formId, questionIndex, "opt0");
  }

  return controlId(formId, questionIndex, "other");
}

export function PermissionCard({
  permission,
  disabled,
  busy,
  onPermission,
}: {
  permission: Permission;
  disabled: boolean;
  busy: boolean;
  onPermission: (optionId: string) => void;
}) {
  const locked = disabled || busy;

  if (permission.options.length === 0) {
    return (
      <Alert className="approval-card bui-approval">
        {permission.title ? (
          <AlertTitle className="wrap-break-word">
            {permission.title}
          </AlertTitle>
        ) : null}
        <AlertDescription className="wrap-break-word">
          没有可用的操作。
        </AlertDescription>
      </Alert>
    );
  }

  return (
    // Approval surface adapted from Beautiful UI ApprovalCard.tsx (MIT, rev 44a274e).
    <Card className="approval-card bui-approval min-w-0">
      {permission.title ? (
        <CardHeader>
          <CardTitle className="wrap-break-word">{permission.title}</CardTitle>
        </CardHeader>
      ) : null}
      <CardFooter className="flex flex-wrap justify-end gap-2">
        {permission.options.map((option, index) => (
          <Button
            key={`${option.id}:${index}`}
            type="button"
            variant={option.allow ? "default" : "outline"}
            disabled={locked}
            className="bui-option h-auto min-h-11 max-w-full whitespace-normal wrap-break-word py-2 text-left"
            onClick={() => onPermission(option.id)}
          >
            {option.label || option.id}
          </Button>
        ))}
      </CardFooter>
    </Card>
  );
}

export function DesktopRequiredAlert() {
  return (
    <Alert className="approval-card bui-approval">
      <AlertTitle>需要桌面端</AlertTitle>
      <AlertDescription className="wrap-break-word">
        审批和回答只能在电脑上完成。
      </AlertDescription>
    </Alert>
  );
}

export function QuestionsCard({
  questions,
  disabled,
  busy,
  onAnswers,
}: {
  questions: Questions;
  disabled: boolean;
  busy: boolean;
  onAnswers: (answers: { questionId: string; answers: string[] }[]) => void;
}) {
  return (
    <QuestionForm
      key={questionKey(questions)}
      questions={questions}
      disabled={disabled}
      busy={busy}
      onAnswers={onAnswers}
    />
  );
}

function QuestionForm({
  questions,
  disabled,
  busy,
  onAnswers,
}: {
  questions: Questions;
  disabled: boolean;
  busy: boolean;
  onAnswers: (answers: { questionId: string; answers: string[] }[]) => void;
}) {
  const formId = React.useId();
  const [answers, setAnswers] = React.useState<AnswerState[]>(() =>
    questions.questions.map(() => ({ selected: [], custom: "" })),
  );
  const [errors, setErrors] = React.useState<Array<string | undefined>>([]);
  // One-question-per-page step nav adapted from Beautiful UI ApprovalCard.tsx
  // (MIT, rev 44a274e). No auto-advance: every answer still goes through the
  // same validation and a single final submit.
  const [step, setStep] = React.useState(0);
  const [reviewing, setReviewing] = React.useState(false);
  const total = questions.questions.length;
  const locked = disabled || busy;
  const pendingFocus = React.useRef<number | null>(null);
  React.useLayoutEffect(() => {
    if (pendingFocus.current === null || reviewing) return;
    const index = pendingFocus.current;
    pendingFocus.current = null;
    focusQuestion(index);
  });

  function clearError(index: number) {
    setErrors((previous) => {
      if (previous[index] === undefined) {
        return previous;
      }

      const next = previous.slice();
      next[index] = undefined;
      return next;
    });
  }

  function setSelected(questionIndex: number, selected: string[]) {
    const question = questions.questions[questionIndex];

    setAnswers((previous) =>
      previous.map((entry, index) => {
        if (index !== questionIndex) {
          return entry;
        }

        return {
          selected,
          custom:
            !question.multiSelect && selected.length > 0 ? "" : entry.custom,
        };
      }),
    );
    clearError(questionIndex);
  }

  function setCustom(questionIndex: number, custom: string) {
    const question = questions.questions[questionIndex];

    setAnswers((previous) =>
      previous.map((entry, index) => {
        if (index !== questionIndex) {
          return entry;
        }

        return {
          selected:
            !question.multiSelect && custom.trim() ? [] : entry.selected,
          custom,
        };
      }),
    );
    clearError(questionIndex);
  }

  function setChecked(
    questionIndex: number,
    optionIndex: number,
    checked: boolean,
  ) {
    const question = questions.questions[questionIndex];
    const value = optionValue(optionIndex);
    const current = answers[questionIndex]?.selected ?? [];
    const selected = question.multiSelect
      ? checked
        ? [...current.filter((entry) => entry !== value), value]
        : current.filter((entry) => entry !== value)
      : checked
        ? [value]
        : [];

    setSelected(questionIndex, selected);
  }

  function resolveAnswers(questionIndex: number) {
    const question = questions.questions[questionIndex];
    const answer = answers[questionIndex] ?? emptyAnswer;
    const picked = question.options.flatMap((option, optionIndex) =>
      answer.selected.includes(optionValue(optionIndex))
        ? [optionText(option, optionIndex)]
        : [],
    );
    const custom = answer.custom.trim();

    if (custom) {
      return Array.from(
        new Set(question.multiSelect ? [...picked, custom] : [custom]),
      );
    }

    return question.multiSelect
      ? Array.from(new Set(picked))
      : picked.slice(0, 1);
  }

  function errorFor(questionIndex: number) {
    const custom = (answers[questionIndex] ?? emptyAnswer).custom.trim();
    const resolved = resolveAnswers(questionIndex);

    if (custom && textBytes(custom) > MAX_ANSWER_BYTES) {
      return "回答过长。";
    }

    if (!resolved.length) {
      return "请回答此问题。";
    }

    if (resolved.length > MAX_ANSWERS) {
      return `最多选择 ${MAX_ANSWERS} 项。`;
    }

    if (resolved.some((answer) => textBytes(answer) > MAX_ANSWER_BYTES)) {
      return "回答过长。";
    }

    return undefined;
  }

  function focusQuestion(questionIndex: number) {
    document
      .getElementById(
        firstControlId(formId, questionIndex, questions.questions[questionIndex]),
      )
      ?.focus();
  }

  function goQuestion(next: number) {
    pendingFocus.current = Math.min(Math.max(next, 0), total - 1);
    setReviewing(false);
    setStep(Math.min(Math.max(next, 0), total - 1));
  }

  function continueFrom(questionIndex: number) {
    if (locked) {
      return;
    }

    const message = errorFor(questionIndex);
    setErrors((previous) => {
      const next = previous.slice();
      next[questionIndex] = message;
      return next;
    });
    if (message) {
      focusQuestion(questionIndex);
      return;
    }

    if (questionIndex < total - 1) {
      pendingFocus.current = questionIndex + 1;
      setStep(questionIndex + 1);
    } else {
      setReviewing(true);
    }
  }

  function editQuestion(questionIndex: number) {
    pendingFocus.current = questionIndex;
    setReviewing(false);
    setStep(questionIndex);
  }

  function submit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();

    if (locked) return;
    if (!reviewing) { continueFrom(step); return; }

    const nextErrors = questions.questions.map((_, index) => errorFor(index));

    setErrors(nextErrors);

    const firstInvalid = nextErrors.findIndex(Boolean);
    if (firstInvalid >= 0) {
      pendingFocus.current = firstInvalid;
      setReviewing(false);
      setStep(firstInvalid);
      return;
    }

    onAnswers(
      questions.questions.map((question, index) => ({
        questionId: question.id,
        answers: resolveAnswers(index),
      })),
    );
  }

  return (
    <form
      noValidate
      aria-busy={busy || undefined}
      onSubmit={submit}
      className="min-w-0"
    >
      <Card className="approval-card min-w-0">
        <CardHeader>
          <CardTitle>等待回答</CardTitle>
        </CardHeader>
        <CardContent className="min-w-0">
          {reviewing ? (
            <div aria-live="polite">
              {questions.questions.map((question, questionIndex) => {
                const resolved = resolveAnswers(questionIndex);
                return (
                  <div
                    key={`${question.id}:${questionIndex}`}
                    className="bui-review-row min-w-0"
                  >
                    <div className="min-w-0 flex-1">
                      <div className="wrap-break-word text-sm font-medium">
                        {question.question ||
                          question.header ||
                          `问题 ${questionIndex + 1}`}
                      </div>
                      <div className="wrap-break-word text-sm text-muted-foreground">
                        {resolved.length ? resolved.join("、") : "未回答"}
                      </div>
                    </div>
                    <Button
                      type="button"
                      variant="outline"
                      disabled={locked}
                      className="min-h-11 shrink-0"
                      onClick={() => editQuestion(questionIndex)}
                    >
                      修改
                    </Button>
                  </div>
                );
              })}
            </div>
          ) : (
          <FieldGroup>
            {questions.questions.map((question, questionIndex) => {
              if (questionIndex !== step) {
                return null;
              }
              const answer = answers[questionIndex] ?? emptyAnswer;
              const error = errors[questionIndex];
              const errorId = controlId(formId, questionIndex, "error");
              const otherId = controlId(formId, questionIndex, "other");
              const useToggleGroup =
                question.options.length >= 2 && question.options.length <= 7;
              const legendId = controlId(formId, questionIndex, "legend");

              return (
                <FieldSet
                  key={`${question.id}:${questionIndex}`}
                  className="min-w-0"
                >
                  <FieldLegend id={legendId} className="wrap-break-word">
                    {question.question ||
                      question.header ||
                      `问题 ${questionIndex + 1}`}
                  </FieldLegend>

                  {useToggleGroup ? (
                    <Field
                      data-invalid={error ? true : undefined}
                      data-disabled={locked || undefined}
                    >
                      <ToggleGroup
                        className="w-full flex-wrap"
                        value={answer.selected}
                        multiple={question.multiSelect}
                        aria-labelledby={legendId}
                        disabled={locked}
                        aria-invalid={error ? true : undefined}
                        aria-describedby={error ? errorId : undefined}
                        onValueChange={(value) =>
                          setSelected(questionIndex, value)
                        }
                      >
                        {question.options.map((option, optionIndex) => (
                          <ToggleGroupItem
                            key={optionValue(optionIndex)}
                            id={controlId(
                              formId,
                              questionIndex,
                              `opt${optionIndex}`,
                            )}
                            value={optionValue(optionIndex)}
                            className="bui-option h-auto min-h-11 max-w-full whitespace-normal wrap-break-word py-2 text-left"
                          >
                            <span className="flex min-w-0 flex-col items-start gap-0.5">
                              <span>{optionText(option, optionIndex)}</span>
                              {option.label && option.description ? (
                                <span className="wrap-break-word text-muted-foreground">
                                  {option.description}
                                </span>
                              ) : null}
                            </span>
                          </ToggleGroupItem>
                        ))}
                      </ToggleGroup>
                    </Field>
                  ) : question.options.length > 0 && !question.multiSelect ? (
                    <RadioGroup
                      value={answer.selected[0] || ""}
                      disabled={locked}
                      aria-labelledby={legendId}
                      onValueChange={(value) =>
                        setSelected(questionIndex, [String(value)])
                      }
                    >
                      {question.options.map((option, optionIndex) => {
                        const id = controlId(
                          formId,
                          questionIndex,
                          `opt${optionIndex}`,
                        );
                        return (
                          <Field
                            key={optionIndex}
                            orientation="horizontal"
                            data-invalid={error ? true : undefined}
                            data-disabled={locked || undefined}
                          >
                            <RadioGroupItem
                              id={id}
                              value={optionValue(optionIndex)}
                              aria-invalid={error ? true : undefined}
                              aria-describedby={error ? errorId : undefined}
                            />
                            <FieldContent>
                              <FieldLabel htmlFor={id}>
                                {optionText(option, optionIndex)}
                              </FieldLabel>
                              {option.label && option.description && (
                                <FieldDescription>
                                  {option.description}
                                </FieldDescription>
                              )}
                            </FieldContent>
                          </Field>
                        );
                      })}
                    </RadioGroup>
                  ) : question.options.length > 0 ? (
                    <FieldGroup className="gap-3">
                      {question.options.map((option, optionIndex) => {
                        const id = controlId(
                          formId,
                          questionIndex,
                          `opt${optionIndex}`,
                        );
                        const value = optionValue(optionIndex);

                        return (
                          <Field
                            key={value}
                            orientation="horizontal"
                            data-invalid={error ? true : undefined}
                            data-disabled={locked || undefined}
                          >
                            <Checkbox
                              id={id}
                              checked={answer.selected.includes(value)}
                              disabled={locked}
                              aria-invalid={error ? true : undefined}
                              aria-describedby={error ? errorId : undefined}
                              onCheckedChange={(checked) =>
                                setChecked(questionIndex, optionIndex, checked)
                              }
                            />
                            <FieldContent>
                              <FieldLabel
                                htmlFor={id}
                                className="max-w-full wrap-break-word"
                              >
                                {optionText(option, optionIndex)}
                              </FieldLabel>
                              {option.label && option.description ? (
                                <FieldDescription className="wrap-break-word">
                                  {option.description}
                                </FieldDescription>
                              ) : null}
                            </FieldContent>
                          </Field>
                        );
                      })}
                    </FieldGroup>
                  ) : null}

                  <Field
                    data-invalid={error ? true : undefined}
                    data-disabled={locked || undefined}
                  >
                    <FieldLabel
                      htmlFor={otherId}
                      className={
                        question.options.length > 0 ? undefined : "sr-only"
                      }
                    >
                      {question.options.length > 0 ? "其他" : "回答"}
                    </FieldLabel>
                    {question.options.length > 0 ? (
                      <Input
                        id={otherId}
                        value={answer.custom}
                        maxLength={MAX_ANSWER_LENGTH}
                        disabled={locked}
                        aria-invalid={error ? true : undefined}
                        aria-describedby={error ? errorId : undefined}
                        onChange={(event) =>
                          setCustom(questionIndex, event.target.value)
                        }
                      />
                    ) : (
                      <Textarea
                        id={otherId}
                        value={answer.custom}
                        rows={3}
                        maxLength={MAX_ANSWER_LENGTH}
                        disabled={locked}
                        aria-invalid={error ? true : undefined}
                        aria-describedby={error ? errorId : undefined}
                        onChange={(event) =>
                          setCustom(questionIndex, event.target.value)
                        }
                      />
                    )}
                  </Field>

                  {error ? <FieldError id={errorId}>{error}</FieldError> : null}
                </FieldSet>
              );
            })}
          </FieldGroup>
          )}
        </CardContent>
        <CardFooter className="justify-between gap-3">
          <div className="bui-step-nav" aria-label="问题导航">
            <button
              type="button"
              aria-label="上一题"
              disabled={locked || (!reviewing && step <= 0)}
              onClick={() =>
                reviewing ? setReviewing(false) : goQuestion(step - 1)
              }
            >
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M18 15l-6-6-6 6" />
              </svg>
            </button>
            <span className="bui-step-counter" aria-live="polite">
              {reviewing ? "检查" : `${step + 1} / ${total}`}
            </span>
            <button
              type="button"
              aria-label="下一题"
              disabled={locked || reviewing || step >= total - 1}
              onClick={() => goQuestion(step + 1)}
            >
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M6 9l6 6 6-6" />
              </svg>
            </button>
          </div>
          {reviewing ? (
            <Button
              type="submit"
              disabled={locked}
              className="min-h-11"
            >
              {busy ? <Spinner data-icon="inline-start" /> : null}
              提交
            </Button>
          ) : (
            <Button
              type="button"
              disabled={locked}
              className="min-h-11"
              onClick={() => continueFrom(step)}
            >
              {step < total - 1 ? "继续" : "检查"}
            </Button>
          )}
        </CardFooter>
      </Card>
    </form>
  );
}
