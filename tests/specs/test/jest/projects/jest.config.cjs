// A CommonJS Jest config with projects.
module.exports = {
  projects: [
    {
      displayName: { name: "alpha", color: "blue" },
      testMatch: ["<rootDir>/a/**/*.test.js"],
    },
    {
      displayName: "beta",
      testMatch: ["<rootDir>/b/**/*.test.js"],
      testEnvironment: "jest-environment-unknown",
    },
  ],
};
