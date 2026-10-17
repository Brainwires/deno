test('adds', () => { expect(1+2).toBe(3); }); test('mock', () => { const f = jest.fn(); f(1); expect(f).toHaveBeenCalledWith(1); });
