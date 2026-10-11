import { feature } from "@/feature";

vi.mock("@/lib/store", () => ({ load: () => "mock-store" }));

it("mocks a module however it is imported", () => {
  expect(feature()).toBe("mock-store@setup-now");
});
