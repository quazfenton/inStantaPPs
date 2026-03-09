// ISA Browser Plugin - Background Service Worker

import { apiService } from './services/api';

// Handle extension messages
chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  handleMessage(message, sender, sendResponse);
  return true; // Keep message channel open for async response
});

async function handleMessage(
  message: any,
  sender: chrome.runtime.MessageSender,
  sendResponse: (response: any) => void
): Promise<void> {
  try {
    switch (message.type) {
      case 'CAPTURE_STATE':
        const captureResult = await apiService.captureState(message.data);
        sendResponse({ success: true, data: captureResult });
        break;

      case 'RESUME_STATE':
        const resumeResult = await apiService.resumeState(message.data);
        sendResponse({ success: true, data: resumeResult });
        break;

      case 'LIST_STATES':
        const listResult = await apiService.listStates();
        sendResponse({ success: true, data: listResult });
        break;

      case 'DELETE_STATE':
        await apiService.deleteState(message.stateId);
        sendResponse({ success: true });
        break;

      case 'FORK_STATE':
        const forkResult = await apiService.forkState(message.stateId, message.label);
        sendResponse({ success: true, data: forkResult });
        break;

      case 'GET_STATUS':
        const statusResult = await apiService.getStatus();
        sendResponse({ success: true, data: statusResult });
        break;

      default:
        sendResponse({ success: false, error: 'Unknown message type' });
    }
  } catch (error) {
    sendResponse({
      success: false,
      error: error instanceof Error ? error.message : 'Unknown error',
    });
  }
}

// Handle extension installation
chrome.runtime.onInstalled.addListener((details) => {
  if (details.reason === 'install') {
    // Open welcome page on first install
    chrome.tabs.create({
      url: 'https://github.com/isa-workspace/isa-workspace#readme',
    });
  }
});

// Periodic state sync (optional)
let syncInterval: NodeJS.Timeout | null = null;

export function startStateSync(intervalMs: number = 60000): void {
  if (syncInterval) {
    clearInterval(syncInterval);
  }

  syncInterval = setInterval(async () => {
    try {
      await apiService.getStatus();
      console.log('ISA: State sync completed');
    } catch (error) {
      console.error('ISA: State sync failed', error);
    }
  }, intervalMs);
}

export function stopStateSync(): void {
  if (syncInterval) {
    clearInterval(syncInterval);
    syncInterval = null;
  }
}

// Start sync if enabled in settings
chrome.storage.local.get(['autoSync'], (result) => {
  if (result.autoSync !== false) {
    startStateSync();
  }
});
