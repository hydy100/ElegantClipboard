/** Share only in-flight work; failures and completed results never become a cache. */
export function singleFlight<T>(work: () => Promise<T>): () => Promise<T> {
  let pending: Promise<T> | undefined;
  return () => {
    if (!pending) {
      pending = Promise.resolve().then(work).finally(() => { pending = undefined; });
    }
    return pending;
  };
}
