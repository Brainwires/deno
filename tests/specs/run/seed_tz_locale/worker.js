import { report } from "./report.js";
self.postMessage(report());
self.close();
