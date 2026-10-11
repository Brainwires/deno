// Vendored from expect-type@1.4.0 (dist/index.js, runtime only) by tools/vendor_vitest.ts. Do not edit.
// Copyright 2024 Misha Kaletsky. Apache-2.0 License, see LICENSE-expect-type.

const fn = () => true;
/**
 * Similar to Jest's `expect`, but with type-awareness.
 * Gives you access to a number of type-matchers that let you make assertions about the
 * form of a reference or generic type parameter.
 *
 * @example
 * ```ts
 * import { foo, bar } from '../foo'
 * import { expectTypeOf } from 'expect-type'
 *
 * test('foo types', () => {
 *   // make sure `foo` has type { a: number }
 *   expectTypeOf(foo).toMatchTypeOf({ a: 1 })
 *   expectTypeOf(foo).toHaveProperty('a').toBeNumber()
 *
 *   // make sure `bar` is a function taking a string:
 *   expectTypeOf(bar).parameter(0).toBeString()
 *   expectTypeOf(bar).returns.not.toBeAny()
 * })
 * ```
 *
 * @description
 * See the [full docs](https://npmjs.com/package/expect-type#documentation) for lots more examples.
 */
const expectTypeOf = (_actual) => {
    const nonFunctionProperties = [
        'parameters',
        'returns',
        'resolves',
        'not',
        'items',
        'constructorParameters',
        'thisParameter',
        'instance',
        'guards',
        'asserts',
    ];
    /* eslint-disable @typescript-eslint/no-unsafe-assignment */
    function createBranded() {
        return {
            inspect: () => true,
            configure: () => createBranded(),
            toEqualTypeOf: fn,
        };
    }
    const obj = {
        toBeAny: fn,
        toBeUnknown: fn,
        toBeNever: fn,
        toBeFunction: fn,
        toBeObject: fn,
        toBeArray: fn,
        toBeString: fn,
        toBeNumber: fn,
        toBeBoolean: fn,
        toBeVoid: fn,
        toBeSymbol: fn,
        toBeNull: fn,
        toBeUndefined: fn,
        toBeNullable: fn,
        toBeBigInt: fn,
        toMatchTypeOf: fn,
        toEqualTypeOf: fn,
        toBeConstructibleWith: fn,
        toMatchObjectType: fn,
        toExtend: fn,
        map: expectTypeOf,
        toBeCallableWith: expectTypeOf,
        extract: expectTypeOf,
        exclude: expectTypeOf,
        pick: expectTypeOf,
        omit: expectTypeOf,
        toHaveProperty: expectTypeOf,
        parameter: expectTypeOf,
        get branded() {
            return createBranded();
        },
    };
    /* eslint-enable @typescript-eslint/no-unsafe-assignment */
    const getterProperties = nonFunctionProperties;
    getterProperties.forEach((prop) => Object.defineProperty(obj, prop, { get: () => expectTypeOf({}) }));
    return obj;
};

export { expectTypeOf };
