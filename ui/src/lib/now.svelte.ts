// A shared ticking clock so relative times and running durations stay live.

let value = $state(Date.now());
setInterval(() => (value = Date.now()), 1000);

export const now = {
  get value() {
    return value;
  },
};
