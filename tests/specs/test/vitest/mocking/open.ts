import { browserName, profile } from "./defaults.ts";

export const open = (): string => `${browserName()}/${profile()}`;
