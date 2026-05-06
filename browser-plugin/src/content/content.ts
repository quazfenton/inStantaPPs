// ISA Browser Plugin - Content Script

// Inject ISA helper into page context
function injectHelper(): void {
  const script = document.createElement('script');
  script.src = chrome.runtime.getURL('injected.js');
  script.onload = function () {
    this.remove();
  };
  (document.head || document.documentElement).appendChild(script);
}

// Listen for messages from popup/background
chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (message.type === 'GET_PAGE_STATE') {
    // Collect page state information
    const pageState = {
      url: window.location.href,
      title: document.title,
      timestamp: new Date().toISOString(),
      cookies: document.cookie,
      localStorage: collectLocalStorage(),
      sessionStorage: collectSessionStorage(),
      scrollPosition: {
        x: window.scrollX,
        y: window.scrollY,
      },
      viewport: {
        width: window.innerWidth,
        height: window.innerHeight,
      },
    };

    sendResponse({ success: true, data: pageState });
  }

  if (message.type === 'RESTORE_PAGE_STATE') {
    // Restore page state
    restorePageState(message.data);
    sendResponse({ success: true });
  }

  return true; // Keep message channel open
});

function collectLocalStorage(): Record<string, string> {
  const data: Record<string, string> = {};
  try {
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key) {
        data[key] = localStorage.getItem(key) || '';
      }
    }
  } catch (e) {
    console.warn('ISA: Could not access localStorage', e);
  }
  return data;
}

function collectSessionStorage(): Record<string, string> {
  const data: Record<string, string> = {};
  try {
    for (let i = 0; i < sessionStorage.length; i++) {
      const key = sessionStorage.key(i);
      if (key) {
        data[key] = sessionStorage.getItem(key) || '';
      }
    }
  } catch (e) {
    console.warn('ISA: Could not access sessionStorage', e);
  }
  return data;
}

function restorePageState(data: any): void {
  // Restore scroll position
  if (data.scrollPosition) {
    window.scrollTo(data.scrollPosition.x, data.scrollPosition.y);
  }

  // Restore localStorage
  if (data.localStorage) {
    Object.entries(data.localStorage).forEach(([key, value]) => {
      try {
        localStorage.setItem(key, value as string);
      } catch (e) {
        console.warn(`ISA: Could not restore localStorage key "${key}"`, e);
      }
    });
  }

  // Restore sessionStorage
  if (data.sessionStorage) {
    Object.entries(data.sessionStorage).forEach(([key, value]) => {
      try {
        sessionStorage.setItem(key, value as string);
      } catch (e) {
        console.warn(`ISA: Could not restore sessionStorage key "${key}"`, e);
      }
    });
  }

  // Dispatch custom event for page-specific restoration
  window.dispatchEvent(
    new CustomEvent('isa-restore', {
      detail: data,
    })
  );
}

// Initialize on page load
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', injectHelper);
} else {
  injectHelper();
}

// Notify background script that content script is ready
chrome.runtime.sendMessage({ type: 'CONTENT_SCRIPT_READY' });
