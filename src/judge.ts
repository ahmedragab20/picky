export interface Question {
  id: string;
  instructions: string;
}

/** Only the call signature: Bun's `typeof fetch` adds statics like `preconnect` that a stub doesn't have. */
export type Fetch = (input: string | URL | Request, init?: RequestInit) => Promise<Response>;

export interface JudgeOptions {
  apiKey: string;
  model: string;
  /** Epoch ms after which no new attempt starts. */
  deadline: number;
  fetchImpl?: Fetch;
  endpoint?: string;
  batchSize?: number;
}

export class JudgeError extends Error {}

export interface JudgeResult {
  /** p(yes) per question id; ids missing here were not answered. */
  answers: Map<string, number>;
  requests: number;
  inputTokens: number;
}

const ENDPOINT = 'https://api.typesafe.ai/v1/systemone';
const RETRYABLE = new Set([429, 500, 502, 503, 504, 529]);
const CRITERIA = {
  true: 'The changes could plausibly change the outcome of these tests.',
  false: 'The changes clearly cannot affect these tests.',
};

/** Asks Jev one yes/no question per candidate against a shared state. */
export async function askJev(state: string, questions: Question[], opts: JudgeOptions): Promise<JudgeResult> {
  const size = opts.batchSize ?? 50;
  const batches: Question[][] = [];
  for (let i = 0; i < questions.length; i += size) batches.push(questions.slice(i, i + size));

  const results = await Promise.all(batches.map((batch) => askBatch(state, batch, opts)));
  return {
    answers: new Map(results.flatMap((r) => [...r.answers])),
    requests: results.length,
    inputTokens: results.reduce((sum, r) => sum + r.inputTokens, 0),
  };
}

async function askBatch(
  state: string,
  batch: Question[],
  opts: JudgeOptions,
): Promise<{ answers: Map<string, number>; inputTokens: number }> {
  const doFetch = opts.fetchImpl ?? fetch;
  const body = JSON.stringify({
    model: opts.model,
    state,
    questions: Object.fromEntries(batch.map((q) => [q.id, { type: 'noul', instructions: q.instructions, criteria: CRITERIA }])),
  });

  for (let attempt = 0; ; attempt++) {
    const remaining = opts.deadline - Date.now();
    if (remaining <= 0) throw new JudgeError('time budget exhausted');

    let res: Response;
    try {
      res = await doFetch(opts.endpoint ?? ENDPOINT, {
        method: 'POST',
        headers: { authorization: `Bearer ${opts.apiKey}`, 'content-type': 'application/json' },
        body,
        signal: AbortSignal.timeout(remaining),
      });
    } catch (err) {
      throw new JudgeError(`request failed: ${(err as Error).message}`);
    }

    if (res.ok) return parseAnswers(await res.json(), batch);
    if (!RETRYABLE.has(res.status)) {
      const detail = (await res.text().catch(() => '')).slice(0, 200);
      throw new JudgeError(`HTTP ${res.status}${detail ? `: ${detail}` : ''}`);
    }
    const backoff = Math.min(8000, 500 * 2 ** attempt);
    if (Date.now() + backoff >= opts.deadline) throw new JudgeError(`HTTP ${res.status}, out of time to retry`);
    await new Promise((r) => setTimeout(r, backoff));
  }
}

function parseAnswers(json: unknown, batch: Question[]): { answers: Map<string, number>; inputTokens: number } {
  const body = json as { answers?: Record<string, { noul?: unknown }>; usage?: { input_tokens?: unknown } };
  if (!body?.answers || typeof body.answers !== 'object') throw new JudgeError('response has no answers');
  const answers = new Map<string, number>();
  for (const q of batch) {
    const p = body.answers[q.id]?.noul;
    if (typeof p === 'number' && Number.isFinite(p) && p >= 0 && p <= 1) answers.set(q.id, p);
  }
  const tokens = body.usage?.input_tokens;
  return { answers, inputTokens: typeof tokens === 'number' ? tokens : 0 };
}
