import { discoveryScenario } from "./discovery.ts";
import { launchScenario } from "./launch.ts";
import { modalScenario } from "./modal.ts";
import { noisyScenario } from "./noisy.ts";
import { recoveryScenario } from "./recovery.ts";
import type { Scenario } from "./types.ts";

export const SCENARIOS: Scenario[] = [
  discoveryScenario,
  launchScenario,
  noisyScenario,
  modalScenario,
  recoveryScenario,
];

export function scenarioById(id: string): Scenario | undefined {
  return SCENARIOS.find((scenario) => scenario.id === id);
}

export * from "./types.ts";
