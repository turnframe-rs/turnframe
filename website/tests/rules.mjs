// The house rules for published copy, as data: a name, a predicate that is true when a string
// breaks the rule, and the reason, which travels with the rule to whoever meets a failure.

export const EM_DASH = {
  name: 'no em dash',
  find: (text) => text.includes('—'),
  why: 'an em dash is the tell of machine-written prose',
};

export const EN_DASH = {
  name: 'no en dash',
  find: (text) => text.includes('–'),
  why: 'a range reads as well with "to", and a dash in prose reads as machine-written',
};

export const NEGATIVE_TRIAD = {
  name: 'no stacked negative triad',
  find: (text) => text.split(/[.!?]\s/).some((sentence) => /\bno\b[^.]*\bno\b[^.]*\bno\b/i.test(sentence)),
  why: 'three negatives in a row is the cadence of machine-written prose and reads as filler',
};

export const RATHER_THAN = {
  name: 'no "rather than"',
  find: (text) => /\brather than\b/i.test(text),
  why: 'say what it is; the contrast is the reader’s to draw',
};

export const WHEN_IT_SHIPS = {
  name: 'nothing says when it ships',
  find: (text) => /\b(soon|shortly|any day now|in the coming (weeks|months)|later this year|next year)\b/i.test(text),
  why: 'a date is a promise, and the vague form is the one that gets written',
};

/** The rules for everything the site itself writes. */
export const SITE = [EM_DASH, EN_DASH, NEGATIVE_TRIAD, RATHER_THAN, WHEN_IT_SHIPS];

export function violations(strings, rules) {
  const found = [];
  for (const [where, text] of strings) {
    for (const rule of rules) if (rule.find(text)) found.push(`${where}: ${rule.name} (${rule.why})\n    ${text.slice(0, 160)}`);
  }
  return found;
}
