// Shared types + data-loading for the quipu board.
// Reads ./data.json at runtime (produced from `qp report` JSON output);
// falls back to an empty payload when the file isn't there.

export type State =
  | 'pending'
  | 'ready'
  | 'assigned'
  | 'running'
  | 'done'
  | 'cancelled';

export const STATES: State[] = [
  'pending',
  'ready',
  'assigned',
  'running',
  'done',
  'cancelled',
];

export const IN_FLIGHT: Set<State> = new Set([
  'pending',
  'ready',
  'assigned',
  'running',
]);

export const STATE_COLOR: Record<State, string> = {
  ready: '#6dd97b',
  pending: '#fbbf24',
  assigned: '#4dcef0',
  running: '#7ab2ff',
  done: '#6b7080',
  cancelled: '#4a4f60',
};

export interface Task {
  id: number;
  display_id: string;
  title: string;
  state: State;
  agent: string | null;
  tags: string[];
  description?: string;
  tier?: string | null;
  blocked_by?: string[];
  last_event?: { ts: string; kind: string; payload?: any } | null;
}

export interface Event {
  id?: number;
  task: string;
  ts: string;
  kind: string;
  agent_id?: string | null;
  payload?: any;
}

export interface Dep {
  from: string;
  to: string;
}

export interface Payload {
  tasks: Task[];
  events: Event[];
  deps: Dep[];
}

export const EMPTY: Payload = { tasks: [], events: [], deps: [] };

export async function loadData(): Promise<Payload> {
  try {
    const res = await fetch('./data.json', { cache: 'no-store' });
    if (!res.ok) return EMPTY;
    const json = (await res.json()) as Partial<Payload>;
    return {
      tasks: json.tasks ?? [],
      events: json.events ?? [],
      deps: json.deps ?? [],
    };
  } catch {
    return EMPTY;
  }
}
