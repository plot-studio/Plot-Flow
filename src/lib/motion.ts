/**
 * Shared motion tokens. Springs only (interruptible, start from the current
 * on-screen value), expressed Apple-style as damping ratio + response:
 * Motion's `bounce: 0` is a critically damped spring (damping 1.0) and
 * `duration` maps to the response in seconds. Nothing in the app overshoots
 * unless a gesture carried momentum — and none of ours do.
 */

/** Default UI spring: damping 1.0, response 0.35 s. Layout, lists, reveals. */
export const SPRING = { type: "spring", bounce: 0, duration: 0.35 } as const;

/** Snappy spring: damping 1.0, response 0.25 s. Indicators, hovers, chips. */
export const SPRING_SNAPPY = {
  type: "spring",
  bounce: 0,
  duration: 0.25,
} as const;

/** Page change: damping 1.0, response 0.3 s, paired with a short fade. */
export const SPRING_PAGE = {
  type: "spring",
  bounce: 0,
  duration: 0.3,
} as const;

/** Press feedback: the scale an element settles to while held. */
export const PRESS_SCALE = 0.97;
