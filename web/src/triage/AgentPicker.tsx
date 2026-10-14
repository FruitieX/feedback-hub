// Agent, model and reasoning-effort selection, limited to what the online
// workers report their CLIs actually offer.

import { RefreshCw, Zap } from 'lucide-react';
import { Tooltip } from 'radix-ui';
import { createContext, useContext, useMemo, useState } from 'react';
import { AgentChoice, AgentInfo, ModelInfo, WorkerStatus } from './types';
import { Option, Select } from './ui';

const DEFAULT = '__default';
const FALLBACK: AgentInfo[] = ['claude', 'codex', 'hermes'].map((name) => ({
  name,
  models: [],
  efforts: [],
  default_model: null,
  models_error: null,
  live_steering: name === 'claude',
}));

export interface WorkersState {
  workers: WorkerStatus[];
  refreshing: boolean;
  refresh: () => void;
}

export const WorkersContext = createContext<WorkersState>({ workers: [], refreshing: false, refresh: () => {} });
export const useWorkers = () => useContext(WorkersContext);

/** What the online workers offer, merged by agent name. */
export function agentOptions(workers: WorkerStatus[]): AgentInfo[] {
  const merged = new Map<string, AgentInfo>();
  for (const worker of workers) {
    for (const agent of worker.agents) {
      const known = merged.get(agent.name);
      if (!known) {
        merged.set(agent.name, { ...agent, models: [...agent.models] });
        continue;
      }
      for (const model of agent.models) if (!known.models.some((m) => m.id === model.id)) known.models.push(model);
      if (!known.efforts.length) known.efforts = agent.efforts;
      known.default_model ??= agent.default_model;
      if (agent.models.length) known.models_error = null;
    }
  }
  return merged.size ? [...merged.values()] : FALLBACK;
}

export const liveSteering = (workers: WorkerStatus[], agent: string) =>
  agentOptions(workers).find((a) => a.name === agent)?.live_steering ?? agent === 'claude';

function modelOf(agent: AgentInfo | undefined, id: string | null): ModelInfo | undefined {
  return id ? agent?.models.find((m) => m.id === id) : undefined;
}

function effortsFor(agent: AgentInfo | undefined, model: ModelInfo | undefined) {
  return model ? model.efforts : (agent?.efforts ?? []);
}

/** Drops choices the agents no longer offer. */
function sanitize(choice: AgentChoice, agents: AgentInfo[]): AgentChoice {
  const agent = agents.find((a) => a.name === choice.agent) ?? agents[0];
  if (!agent) return choice;
  const known = agent.models.length > 0;
  const model = known ? modelOf(agent, choice.model) : undefined;
  const modelId = known ? (model?.id ?? null) : null;
  const efforts = effortsFor(agent, model);
  const effort = choice.effort && efforts.includes(choice.effort) ? choice.effort : null;
  return { agent: agent.name, model: modelId, effort };
}

/** The owner's last agent choice, kept by the hub so every device (and the widget) shares it. */
export const ChoiceContext = createContext<{ last: AgentChoice | null; remember: (choice: AgentChoice) => void }>({
  last: null,
  remember: () => {},
});

/** Starts from the last queued choice; edits stay local until something is queued. */
export function useAgentChoice(): [AgentChoice, (choice: AgentChoice) => void] {
  const { workers } = useWorkers();
  const { last } = useContext(ChoiceContext);
  const agents = useMemo(() => agentOptions(workers), [workers]);
  const [edited, setEdited] = useState<AgentChoice | null>(null);
  const valid = useMemo(
    () => sanitize(edited ?? last ?? { agent: 'claude', model: null, effort: null }, agents),
    [edited, last, agents],
  );
  return [valid, setEdited];
}

export function AgentPicker({ value, onChange }: { value: AgentChoice; onChange: (choice: AgentChoice) => void }) {
  const { workers, refresh, refreshing } = useWorkers();
  const agents = useMemo(() => agentOptions(workers), [workers]);
  const agent = agents.find((a) => a.name === value.agent);
  const model = modelOf(agent, value.model);
  const efforts = effortsFor(agent, model);

  const agentItems: Option[] = agents.map((a) => ({
    value: a.name,
    label: a.name,
    hint: a.live_steering ? 'live steering' : undefined,
  }));
  const modelItems: Option[] = [
    { value: DEFAULT, label: agent?.default_model ? `Default · ${agent.default_model}` : 'Default model' },
    ...(agent?.models ?? []).map((m) => ({ value: m.id, label: m.name, hint: m.description ?? undefined })),
  ];
  const defaultEffort = model?.default_effort;
  const effortItems: Option[] = [
    { value: DEFAULT, label: defaultEffort ? `Default · ${defaultEffort}` : 'Default effort' },
    ...efforts.map((e) => ({ value: e, label: e })),
  ];
  const noEffort = !!model && efforts.length === 0;

  const setModel = (id: string) => {
    const next = id === DEFAULT ? undefined : modelOf(agent, id);
    const nextEfforts = effortsFor(agent, next);
    onChange({
      agent: value.agent,
      model: next?.id ?? null,
      effort: value.effort && nextEfforts.includes(value.effort) ? value.effort : null,
    });
  };

  const modelsUnknown = !agent?.models.length;
  const modelsHint = !workers.length
    ? 'No worker online: model lists appear once a worker checks in.'
    : agent?.models_error
      ? `Could not list models: ${agent.models_error}`
      : modelsUnknown
        ? 'This agent reports no model list; its default model is used.'
        : null;

  return (
    <div className="picker">
      <Select
        label="Agent"
        value={value.agent}
        options={agentItems}
        onChange={(name) => onChange({ agent: name, model: null, effort: null })}
        className="picker-agent"
      />
      <Select
        label="Model"
        value={value.model ?? DEFAULT}
        options={modelItems}
        onChange={setModel}
        disabled={modelsUnknown}
        className="picker-model"
      />
      <Select
        label="Reasoning effort"
        value={value.effort ?? DEFAULT}
        options={noEffort ? [{ value: DEFAULT, label: 'No effort setting' }] : effortItems}
        onChange={(e) => onChange({ ...value, effort: e === DEFAULT ? null : e })}
        disabled={noEffort || (efforts.length === 0 && modelsUnknown)}
        className="picker-effort"
      />
      <Tooltip.Root>
        <Tooltip.Trigger asChild>
          <button
            type="button"
            className="icon-button"
            onClick={refresh}
            disabled={refreshing || !workers.length}
            aria-label="Refresh model lists"
          >
            <RefreshCw size={15} className={refreshing ? 'spin' : ''} />
          </button>
        </Tooltip.Trigger>
        <Tooltip.Portal>
          <Tooltip.Content className="tooltip" sideOffset={4}>
            Ask the workers to re-read their model lists
          </Tooltip.Content>
        </Tooltip.Portal>
      </Tooltip.Root>
      {modelsHint && <p className="picker-hint">{modelsHint}</p>}
      {agent?.live_steering && (
        <p className="picker-hint subtle">
          <Zap size={12} /> You can message {agent.name} while it works.
        </p>
      )}
    </div>
  );
}

export const choiceLabel = (c: { agent: string; model: string | null; effort: string | null }) =>
  [c.agent, c.model, c.effort].filter(Boolean).join(' · ');
