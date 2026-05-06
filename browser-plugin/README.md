# ISA Browser Plugin

A browser extension for capturing and managing ISA states directly from your browser.

## Features

- **Capture State**: Snapshot current web app state with one click
- **Resume State**: Restore captured states in browser
- **State Browser**: View and manage all your states
- **Share States**: Generate shareable links
- **Real-time Sync**: WebSocket connection for live updates

## Installation

### Development

```bash
cd browser-plugin
npm install
npm run build:dev
```

### Load in Browser

**Chrome/Edge:**
1. Go to `chrome://extensions/`
2. Enable "Developer mode"
3. Click "Load unpacked"
4. Select `dist/` folder

**Firefox:**
1. Go to `about:debugging`
2. Click "Load Temporary Add-on"
3. Select `manifest.json`

## Usage

### Capture State

1. Navigate to any web app
2. Click ISA extension icon
3. Click "Capture State"
4. State is saved to ISA server

### Resume State

1. Click ISA extension icon
2. Select state from list
3. Click "Resume"
4. State loads in new tab

### Share State

1. Open state details
2. Click "Share"
3. Copy link or QR code

## Project Structure

```
browser-plugin/
├── src/
│   ├── popup/          # Extension popup UI
│   ├── background/     # Background service worker
│   ├── content/        # Content scripts
│   ├── components/     # React components
│   ├── services/       # API services
│   └── utils/          # Utilities
├── public/
│   ├── manifest.json   # Extension manifest
│   └── icons/          # Extension icons
├── package.json
└── webpack.config.js
```

## API Integration

The plugin communicates with the ISA server via:

```typescript
// Capture state
POST /v1/snapshot
{
  "label": "My Web App",
  "ttl": "24h",
  "metadata": {
    "url": "https://example.com",
    "timestamp": "2024-01-15T10:30:00Z"
  }
}

// Resume state
POST /v1/resume
{
  "state_id": "uuid",
  "mode": "browser"
}
```

## Development

```bash
# Install dependencies
npm install

# Development build with watch
npm run build:dev

# Production build
npm run build:prod

# Run tests
npm test

# Lint
npm run lint
```

## Requirements

- Node.js 18+
- ISA Server running at `http://localhost:3000`

## Configuration

Edit `src/config.ts`:

```typescript
export const config = {
  apiBaseUrl: 'http://localhost:3000',
  wsUrl: 'ws://localhost:3000/ws',
  captureTimeout: 30000,
};
```

## License

MIT
