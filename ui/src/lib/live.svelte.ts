// per-session change counter, bumped by the /api/watch SSE stream in App
export const live = $state({ tick: {} as Record<string, number> })
