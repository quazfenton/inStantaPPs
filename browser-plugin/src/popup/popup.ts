import { apiService } from '../services/api';
import type { StateSummary } from '../config';

class Popup {
  private captureBtn: HTMLButtonElement;
  private refreshBtn: HTMLButtonElement;
  private statesList: HTMLElement;
  private messageDiv: HTMLElement;

  constructor() {
    this.captureBtn = document.getElementById('captureBtn') as HTMLButtonElement;
    this.refreshBtn = document.getElementById('refreshBtn') as HTMLButtonElement;
    this.statesList = document.getElementById('statesList') as HTMLElement;
    this.messageDiv = document.getElementById('message') as HTMLElement;

    this.init();
  }

  private init(): void {
    this.captureBtn.addEventListener('click', () => this.handleCapture());
    this.refreshBtn.addEventListener('click', () => this.loadStates());

    this.loadStates();
  }

  private async handleCapture(): Promise<void> {
    this.setLoading(true);
    this.clearMessage();

    try {
      // Get current tab info
      const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });

      if (!tab?.url) {
        this.showError('Cannot capture state: No URL available');
        return;
      }

      // Capture state
      const result = await apiService.captureState({
        label: tab.title || 'Untitled',
        ttl: '24h',
        metadata: {
          url: tab.url,
          timestamp: new Date().toISOString(),
          title: tab.title || 'Untitled',
        },
      });

      this.showSuccess(`State captured! ID: ${result.state_id.slice(0, 8)}...`);
      this.loadStates();
    } catch (error) {
      this.showError(error instanceof Error ? error.message : 'Failed to capture state');
    } finally {
      this.setLoading(false);
    }
  }

  private async loadStates(): Promise<void> {
    this.statesList.innerHTML = '<div class="loading">Loading states...</div>';

    try {
      const data = await apiService.listStates();

      if (data.states.length === 0) {
        this.statesList.innerHTML = `
          <div class="empty-state">
            <p>📭 No states yet</p>
            <p>Capture your first state above!</p>
          </div>
        `;
        return;
      }

      this.statesList.innerHTML = data.states
        .map((state) => this.renderStateItem(state))
        .join('');

      // Add click handlers
      this.statesList.querySelectorAll('.state-item').forEach((item) => {
        item.addEventListener('click', async () => {
          const stateId = item.getAttribute('data-state-id');
          if (stateId) {
            await this.handleResume(stateId);
          }
        });
      });
    } catch (error) {
      this.statesList.innerHTML = `
        <div class="empty-state">
          <p>❌ Failed to load states</p>
          <p>Make sure ISA server is running</p>
        </div>
      `;
    }
  }

  private renderStateItem(state: StateSummary): string {
    const date = new Date(state.created_at).toLocaleDateString();
    return `
      <div class="state-item" data-state-id="${state.state_id}">
        <h3>${this.escapeHtml(state.label)}</h3>
        <p>${state.url || 'No URL'} • ${date}</p>
      </div>
    `;
  }

  private async handleResume(stateId: string): Promise<void> {
    this.setLoading(true);
    this.clearMessage();

    try {
      await apiService.resumeState({
        state_id: stateId,
        mode: 'browser',
      });

      this.showSuccess('State resumed! Opening in new tab...');

      // Open in new tab (ISA server will handle the rest)
      await chrome.tabs.create({
        url: `${chrome.runtime.getURL('resume.html')}?state_id=${stateId}`,
      });
    } catch (error) {
      this.showError(error instanceof Error ? error.message : 'Failed to resume state');
    } finally {
      this.setLoading(false);
    }
  }

  private showSuccess(message: string): void {
    this.messageDiv.innerHTML = `<div class="success">${this.escapeHtml(message)}</div>`;
    setTimeout(() => this.clearMessage(), 5000);
  }

  private showError(message: string): void {
    this.messageDiv.innerHTML = `<div class="error">${this.escapeHtml(message)}</div>`;
    setTimeout(() => this.clearMessage(), 5000);
  }

  private clearMessage(): void {
    this.messageDiv.innerHTML = '';
  }

  private setLoading(loading: boolean): void {
    this.captureBtn.disabled = loading;
    this.refreshBtn.disabled = loading;
  }

  private escapeHtml(text: string): string {
    const div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
  }
}

// Initialize popup
new Popup();
