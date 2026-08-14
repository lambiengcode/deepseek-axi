// Small deliberately-imperfect fixture file used by the review bench task.
export function divide(a, b) {
  return a / b; // no zero-check
}

export function parseConfig(raw) {
  return JSON.parse(raw); // no try/catch
}
