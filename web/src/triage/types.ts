export interface Feedback {
  id: number;
  app: string;
  kind: string;
  message: string;
  reporter: string | null;
  page_url: string | null;
  page_title: string | null;
  user_agent: string | null;
  source: string;
  context: Record<string, unknown>;
  status: string;
  agent: string | null;
  attempts: number;
  screenshot_count: number;
  created_at: string;
  updated_at: string;
}

export interface Screenshot {
  idx: number;
  mime: string;
  size: number;
  note: string | null;
}

export interface FeedbackEvent {
  id: number;
  at: string;
  author: string;
  kind: string;
  body: string;
}

export interface Detail extends Feedback {
  screenshots: Screenshot[];
  events: FeedbackEvent[];
  runs: Run[];
}

export interface Run {
  id: number;
  feedback_id: number;
  agent: string;
  model: string | null;
  effort: string | null;
  note: string | null;
  status: string;
  worker: string | null;
  workdir: string | null;
  branch: string | null;
  session_id: string | null;
  log_path: string | null;
  summary: string | null;
  cost_usd: number | null;
  tokens: number | null;
  requested_by: string;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  updated_at: string;
  app: string;
  title: string;
  feedback_status: string;
  pending_messages: number;
}

export interface RunEvent {
  id: number;
  at: string;
  kind: string;
  body: string;
}

export interface RunDetail extends Run {
  events: RunEvent[];
}

export interface AppInfo {
  id: string;
  name?: string;
  repo?: string;
}

export interface ModelInfo {
  id: string;
  name: string;
  description: string | null;
  efforts: string[];
  default_effort: string | null;
}

export interface AgentInfo {
  name: string;
  models: ModelInfo[];
  efforts: string[];
  default_model: string | null;
  models_error: string | null;
  live_steering: boolean;
}

export interface WorkerStatus {
  name: string;
  agents: AgentInfo[];
  running: number[];
  max_parallel: number;
  last_seen: string;
}

export interface AgentChoice {
  agent: string;
  model: string | null;
  effort: string | null;
}

export const STATUSES = ['open', 'queued', 'in_progress', 'resolved', 'dismissed'] as const;
export const ACTIVE_RUN = ['queued', 'running'];
export const KINDS: Record<string, string> = { bug: 'Problem', idea: 'Idea', other: 'Other' };
