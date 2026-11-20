import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { DesktopLyricsApp } from './DesktopLyricsApp';

const rootEl = document.getElementById('lyrics-root');
if (rootEl) {
  createRoot(rootEl).render(
    <StrictMode>
      <DesktopLyricsApp />
    </StrictMode>
  );
}
