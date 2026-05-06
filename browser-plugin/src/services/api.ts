import { config, type StateSummary, type CaptureRequest, type ResumeRequest } from './config';

export class ApiService {
  private baseUrl: string;

  constructor(baseUrl: string = config.apiBaseUrl) {
    this.baseUrl = baseUrl;
  }

  async captureState(request: CaptureRequest): Promise<{ state_id: string }> {
    const response = await fetch(`${this.baseUrl}/v1/snapshot`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(request),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(error.error || 'Failed to capture state');
    }

    return response.json();
  }

  async resumeState(request: ResumeRequest): Promise<{ accepted: boolean; target_region: string }> {
    const response = await fetch(`${this.baseUrl}/v1/resume`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(request),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(error.error || 'Failed to resume state');
    }

    return response.json();
  }

  async listStates(): Promise<{ count: number; states: StateSummary[] }> {
    const response = await fetch(`${this.baseUrl}/v1/list`, {
      method: 'GET',
    });

    if (!response.ok) {
      throw new Error('Failed to list states');
    }

    return response.json();
  }

  async deleteState(stateId: string): Promise<void> {
    const response = await fetch(`${this.baseUrl}/v1/delete`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({ state_id: stateId }),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(error.error || 'Failed to delete state');
    }
  }

  async forkState(stateId: string, label?: string): Promise<{ state_id: string }> {
    const response = await fetch(`${this.baseUrl}/v1/fork`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({ state_id: stateId, label }),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(error.error || 'Failed to fork state');
    }

    return response.json();
  }

  async getStatus(): Promise<{ status: string; stored_states: number }> {
    const response = await fetch(`${this.baseUrl}/v1/status`, {
      method: 'POST',
    });

    if (!response.ok) {
      throw new Error('Failed to get status');
    }

    return response.json();
  }
}

export const apiService = new ApiService();
