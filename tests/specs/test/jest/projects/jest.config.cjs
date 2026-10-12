// A CommonJS Jest config with projects.
module.exports = {
  projects: [
    {
      displayName: { name: "alpha", color: "blue" },
      testMatch: ["<rootDir>/a/**/*.test.js", "<rootDir>/shared/**/*.test.js"],
    },
    {
      displayName: "beta",
      testMatch: ["<rootDir>/b/**/*.test.js", "<rootDir>/shared/**/*.test.js"],
      testEnvironment: "jest-environment-unknown",
    },
  ],
};
