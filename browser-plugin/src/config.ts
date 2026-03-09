export const config = {
  apiBaseUrl: 'http://localhost:3000',
  wsUrl: 'ws://localhost:3000/ws',
  captureTimeout: 30000,
  maxStates: 100,
};

export type StateSummary = {
  state_id: string;
  label: string;
  created_at: string;
  url?: string;
};

export type CaptureRequest = {
  label: string;
  ttl: string;
  metadata: {
    url: string;
    timestamp: string;
    title: string;
  };
};

export type ResumeRequest = {
  state_id: string;
  mode: string;
};
