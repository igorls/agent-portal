/**
 * Approximate public list prices (USD per 1M tokens) for cost estimates.
 * Matched against normalized model ids from session peeks. Rates are
 * intentionally coarse — real bills include cache hits, batch discounts,
 * and subscription plans these numbers ignore.
 */

export interface ModelRate {
  /** USD per 1M input tokens */
  inputPerM: number;
  /** USD per 1M output tokens */
  outputPerM: number;
}

/**
 * Agentic coding sessions are input-heavy (tool results + history re-sent).
 * Used only when we lack true input/output split from the store.
 */
export const AGENTIC_INPUT_SHARE = 0.85;
export const AGENTIC_OUTPUT_SHARE = 0.15;

/** First matching pattern wins. Patterns run against lowercased model ids. */
const RATES: { re: RegExp; rate: ModelRate }[] = [
  // Anthropic
  { re: /claude-opus-4[.-]?1\b|claude-opus-4(?![.-]?[5-9])/, rate: { inputPerM: 15, outputPerM: 75 } },
  { re: /claude-opus/, rate: { inputPerM: 5, outputPerM: 25 } },
  { re: /claude-sonnet/, rate: { inputPerM: 3, outputPerM: 15 } },
  { re: /claude-haiku/, rate: { inputPerM: 1, outputPerM: 5 } },
  { re: /claude/, rate: { inputPerM: 3, outputPerM: 15 } },
  // OpenAI / Codex
  { re: /gpt-5\.6-sol|gpt-5-6-sol/, rate: { inputPerM: 5, outputPerM: 30 } },
  { re: /gpt-5\.5|gpt-5-5/, rate: { inputPerM: 5, outputPerM: 30 } },
  { re: /gpt-5\.4|gpt-5-4/, rate: { inputPerM: 2.5, outputPerM: 15 } },
  { re: /gpt-5\.3|gpt-5-3|codex/, rate: { inputPerM: 1.75, outputPerM: 14 } },
  { re: /gpt-5\.?o-mini|gpt-5-mini|o4-mini|o3-mini/, rate: { inputPerM: 1.1, outputPerM: 4.4 } },
  { re: /gpt-5|o3\b|o4\b/, rate: { inputPerM: 5, outputPerM: 15 } },
  { re: /gpt-4o-mini/, rate: { inputPerM: 0.15, outputPerM: 0.6 } },
  { re: /gpt-4o/, rate: { inputPerM: 2.5, outputPerM: 10 } },
  { re: /gpt-4\.1|gpt-4-1/, rate: { inputPerM: 2, outputPerM: 8 } },
  { re: /gpt-4/, rate: { inputPerM: 10, outputPerM: 30 } },
  // xAI / Grok
  { re: /grok-4\.?1|grok-4-1/, rate: { inputPerM: 0.2, outputPerM: 0.5 } },
  { re: /grok-4\.?5|grok-4-5|grok-build/, rate: { inputPerM: 1.8, outputPerM: 7.2 } },
  { re: /grok-3|grok/, rate: { inputPerM: 3, outputPerM: 15 } },
  // Google
  { re: /gemini-.*flash|gemini-2\.5-flash|gemini-3-flash/, rate: { inputPerM: 0.5, outputPerM: 3 } },
  { re: /gemini/, rate: { inputPerM: 2, outputPerM: 12 } },
  // Local / free-ish
  { re: /ollama|local|qwen|llama|deepseek|glm|mistral/, rate: { inputPerM: 0, outputPerM: 0 } },
];

export function rateForModel(model: string | null | undefined): ModelRate | null {
  if (!model || model === 'unknown') return null;
  const id = model.toLowerCase();
  for (const { re, rate } of RATES) {
    if (re.test(id)) return rate;
  }
  return null;
}

/**
 * Rough token estimate from on-disk session size.
 * Store files are JSON-heavy; we treat size as ~character volume at 4 chars/token
 * (same order of magnitude as portal-core's content estimate).
 */
export function estimateTokensFromBytes(bytes: number): number {
  if (bytes <= 0) return 0;
  return Math.round(bytes / 4);
}

/** USD cost for an estimated token mass with agentic I/O mix. */
export function estimateCostUsd(tokens: number, rate: ModelRate | null): number | null {
  if (!rate || tokens <= 0) return rate && rate.inputPerM === 0 && rate.outputPerM === 0 ? 0 : null;
  const input = tokens * AGENTIC_INPUT_SHARE;
  const output = tokens * AGENTIC_OUTPUT_SHARE;
  return (input * rate.inputPerM + output * rate.outputPerM) / 1_000_000;
}

export function formatUsd(n: number | null | undefined): string {
  if (n == null || Number.isNaN(n)) return '—';
  if (n === 0) return '$0';
  if (n < 0.01) return '<$0.01';
  if (n < 10) return `$${n.toFixed(2)}`;
  if (n < 1000) return `$${n.toFixed(0)}`;
  if (n < 10_000) return `$${(n / 1000).toFixed(1)}k`;
  return `$${Math.round(n / 1000)}k`;
}

export function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10_000) return `${(n / 1000).toFixed(1)}k`;
  if (n < 1_000_000) return `${Math.round(n / 1000)}k`;
  if (n < 10_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  return `${Math.round(n / 1_000_000)}M`;
}
