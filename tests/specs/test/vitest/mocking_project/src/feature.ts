import { load } from "./lib/store";
import { now } from "../src/lib/clock.ts";

export const feature = (): string => `${load()}@${now()}`;
