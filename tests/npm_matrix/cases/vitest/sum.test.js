import { test, expect, vi } from 'vitest'; test('adds', () => { expect(1+2).toBe(3); const f = vi.fn(); f(); expect(f).toHaveBeenCalled(); });
