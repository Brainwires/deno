/** A Jest config: deno test maps it onto its Vitest layer. */
export default {
  displayName: "spec",
  roots: ["<rootDir>/src"],
  testMatch: ["**/__tests__/**/*.(spec|test).ts"],
  testPathIgnorePatterns: ["/node_modules/", "/ignored/"],
  moduleNameMapper: {
    "^@lib/(.*)$": "<rootDir>/lib/$1",
    "\\.css$": "<rootDir>/style_mock.js",
  },
  moduleDirectories: ["node_modules", "shared"],
  setupFiles: ["<rootDir>/setup.ts"],
  setupFilesAfterEnv: ["<rootDir>/setup_after_env.ts"],
  testEnvironment: "node",
  globals: { __APP__: { name: "jest" } },
  restoreMocks: true,
  testTimeout: 1234,
  transform: {
    "^.+\\.tsx?$": "ts-jest",
    "^.+\\.svg$": "<rootDir>/svg_transform.js",
  },
  snapshotSerializers: ["<rootDir>/serializer.js"],
};
