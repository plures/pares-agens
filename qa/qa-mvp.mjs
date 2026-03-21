/**
 * Pares Agens MVP 0.3.0 — QA Test Suite
 *
 * Agent-driven runtime testing of the live UI.
 * Tests the application the way a human end-user would interact with it.
 *
 * Uses Playwright to launch the built Svelte app and exercise every screen.
 * Tauri invoke() calls are mocked since we're testing outside the Tauri runtime.
 */
import { chromium } from 'playwright';
import { createServer } from 'http';
import { readFileSync, existsSync } from 'fs';
import { join, extname } from 'path';

const DIST = join(import.meta.dirname, '..', 'crates', 'tauri-app', 'ui', 'dist');
const PORT = 4199;
const RESULTS = [];

// ── Helpers ────────────────────────────────────────────────────────────────
const MIME = {
  '.html': 'text/html',
  '.js': 'application/javascript',
  '.css': 'text/css',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
};

function startServer() {
  return new Promise((resolve) => {
    const srv = createServer((req, res) => {
      let filePath = join(DIST, req.url === '/' ? 'index.html' : req.url);
      if (!existsSync(filePath)) filePath = join(DIST, 'index.html'); // SPA fallback
      const ext = extname(filePath);
      res.writeHead(200, { 'Content-Type': MIME[ext] || 'application/octet-stream' });
      res.end(readFileSync(filePath));
    });
    srv.listen(PORT, () => resolve(srv));
  });
}

function record(name, pass, detail = '') {
  RESULTS.push({ name, pass, detail });
  const icon = pass ? '✅' : '❌';
  console.log(`  ${icon} ${name}${detail ? ' — ' + detail : ''}`);
}

// ── QA Tests ───────────────────────────────────────────────────────────────
async function runQA() {
  console.log('\n🧪 Pares Agens MVP 0.3.0 — QA Test Suite\n');

  // Verify build exists
  if (!existsSync(join(DIST, 'index.html'))) {
    console.error('❌ FATAL: No build found at', DIST);
    process.exit(1);
  }

  const server = await startServer();
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ viewport: { width: 1200, height: 800 } });

  // Mock Tauri APIs before page load
  await context.addInitScript(() => {
    // Mark wizard as completed so we can test the main UI
    localStorage.setItem('wizard_completed', '1');
    
    // Mock Tauri core
    window.__TAURI__ = {
      core: {
        invoke: async (cmd, args) => {
          console.log('[MOCK] invoke:', cmd, JSON.stringify(args ?? {}));
          // Return sensible defaults for each command
          const mocks = {
            get_settings: {
              providers: [
                { name: 'openai', baseUrl: 'https://api.openai.com/v1', apiKey: 'sk-test', models: ['gpt-4'] },
              ],
              routing: { default_provider: 'openai', model_map: {} },
              mcpServers: [],
            },
            set_settings: true,
            get_memories: [
              { id: '1', content: 'Test memory entry', category: 'preference', timestamp: Date.now(), tags: ['test'] },
              { id: '2', content: 'Another memory', category: 'decision', timestamp: Date.now(), tags: [] },
            ],
            get_praxis_guidance: [],
            get_analysis_events: [],
            trigger_praxis_analysis: true,
            list_mcp_tools: [],
            list_providers: [
              { name: 'openai', baseUrl: 'https://api.openai.com/v1', models: ['gpt-4'] },
            ],
            restart_mcp_servers: true,
            send_message: { role: 'assistant', content: 'Hello! I am Pares Agens.' },
            get_history: [],
            check_setup: { needs_setup: false },
          };
          return mocks[cmd] ?? null;
        },
      },
      event: {
        listen: async () => () => {},
        emit: async () => {},
      },
    };
  });

  const page = await context.newPage();
  const consoleErrors = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error' && !msg.text().includes('[MOCK]')) {
      consoleErrors.push(msg.text());
      console.log(`  [CONSOLE ERROR] ${msg.text()}`);
    }
  });
  page.on('pageerror', (err) => consoleErrors.push(err.message));

  try {
    // ── T1: App loads without crash ──────────────────────────────────────
    console.log('─── Screen: Initial Load ───');
    await page.goto(`http://localhost:${PORT}`, { waitUntil: 'networkidle' });
    await page.waitForTimeout(1000); // Let Svelte hydrate
    const title = await page.title();
    record('T1: App loads', true, `title="${title}"`);

    // ── T2: No JS errors on load ─────────────────────────────────────────
    const loadErrors = consoleErrors.filter(e =>
      !e.includes('invoke') && !e.includes('__TAURI__') && !e.includes('[MOCK]')
    );
    record('T2: No JS errors on load', loadErrors.length === 0,
      loadErrors.length > 0 ? `${loadErrors.length} errors: ${loadErrors[0]}` : 'clean');

    // ── T3: Main UI visible (chat or wizard) ─────────────────────────────
    const hasMainContent = await page.locator('main, .chat-container, .wizard, .app-layout, [role="main"]').first().isVisible().catch(() => false);
    const bodyText = await page.locator('body').innerText().catch(() => '');
    record('T3: Main UI visible', hasMainContent || bodyText.length > 0,
      hasMainContent ? 'main content found' : `body text length: ${bodyText.length}`);

    // ── T4: Chat input exists and accepts text ───────────────────────────
    console.log('─── Screen: Chat ───');
    const chatInput = page.locator('textarea, input[type="text"], [contenteditable], .chat-input, [placeholder*="message" i]').first();
    const chatInputExists = await chatInput.isVisible({ timeout: 3000 }).catch(() => false);
    if (chatInputExists) {
      await chatInput.fill('Hello, Pares Agens!');
      const value = await chatInput.inputValue().catch(async () => await chatInput.innerText().catch(() => ''));
      record('T4: Chat input accepts text', value.includes('Hello'), value);
    } else {
      record('T4: Chat input accepts text', false, 'Chat input not found — may be on wizard screen');
    }

    // ── T5: Send button exists ───────────────────────────────────────────
    const sendBtn = page.locator('button:has-text("Send"), button[aria-label*="send" i], button.send-button, form button[type="submit"]').first();
    const sendExists = await sendBtn.isVisible({ timeout: 2000 }).catch(() => false);
    record('T5: Send button visible', sendExists);

    // ── T6: Settings dialog opens ────────────────────────────────────────
    console.log('─── Screen: Settings ───');
    const settingsBtn = page.locator('button:has-text("Settings"), button[aria-label*="settings" i], [title*="Settings" i], button:has-text("⚙")').first();
    const settingsExists = await settingsBtn.isVisible({ timeout: 2000 }).catch(() => false);
    if (settingsExists) {
      await settingsBtn.click();
      await page.waitForTimeout(500);
      const dialog = page.locator('dialog[open], [role="dialog"], .settings-dialog, .dialog-overlay');
      const dialogOpen = await dialog.first().isVisible({ timeout: 2000 }).catch(() => false);
      record('T6: Settings dialog opens', dialogOpen);

      if (dialogOpen) {
        // ── T7: Settings tabs navigable ────────────────────────────────────
        const tabs = page.locator('[role="tab"], .settings-tab, button[id^="tab-"]');
        const tabCount = await tabs.count();
        record('T7: Settings has tabs', tabCount >= 3, `${tabCount} tabs found`);

        // ── T8: MCP tab exists ───────────────────────────────────────────
        const mcpTab = page.locator('[role="tab"]:has-text("Mcp"), [id="tab-mcp"]').first();
        const mcpTabExists = await mcpTab.isVisible({ timeout: 1000 }).catch(() => false);
        record('T8: MCP tab exists', mcpTabExists);

        if (mcpTabExists) {
          // ── T9: MCP tab opens ──────────────────────────────────────────
          await mcpTab.click({ force: true });
          await page.waitForTimeout(800);
          const mcpPanel = page.locator('#panel-mcp');
          // In Svelte, hidden attr is present as "" when true, absent when false
          const mcpHiddenAttr = await mcpPanel.getAttribute('hidden');
          const mcpVisible = mcpHiddenAttr === null;
          record('T9: MCP panel visible', mcpVisible, mcpHiddenAttr === null ? 'shown' : `hidden attr present`);

          // ── T10: Add Server button exists ──────────────────────────────
          const addServerBtn = page.locator('#panel-mcp button:has-text("Add"), button:has-text("Add Server")').first();
          const addServerExists = await addServerBtn.isVisible({ timeout: 1000 }).catch(() => false);
          record('T10: Add Server button exists', addServerExists);

          if (addServerExists) {
            // ── T11: MCP form opens ────────────────────────────────────────
            await addServerBtn.click();
            await page.waitForTimeout(300);
            const mcpForm = page.locator('.mcp-form, form:has(input)').first();
            const formVisible = await mcpForm.isVisible({ timeout: 1000 }).catch(() => false);
            record('T11: MCP server form opens', formVisible);
          } else {
            record('T11: MCP server form opens', false, 'Add button not found');
          }
        } else {
          record('T9: MCP panel visible', false, 'MCP tab not found');
          record('T10: Add Server button exists', false, 'MCP tab not found');
          record('T11: MCP server form opens', false, 'MCP tab not found');
        }

        // ── T12: Each tab is clickable ─────────────────────────────────────
        let allTabsWork = true;
        for (let i = 0; i < tabCount; i++) {
          const tab = tabs.nth(i);
          const tabName = await tab.innerText().catch(() => `Tab ${i}`);
          try {
            await tab.click();
            await page.waitForTimeout(200);
          } catch {
            allTabsWork = false;
          }
        }
        record('T12: All tabs clickable', allTabsWork, `${tabCount} tabs`);

        // Close dialog
        const cancelBtn = page.locator('button:has-text("Cancel")').first();
        if (await cancelBtn.isVisible().catch(() => false)) {
          await cancelBtn.click();
          await page.waitForTimeout(300);
        }
      } else {
        record('T7: Settings has tabs', false, 'Dialog did not open');
        record('T8: MCP tab exists', false, 'Dialog did not open');
        record('T9-T12', false, 'Dialog did not open');
      }
    } else {
      record('T6: Settings dialog opens', false, 'Settings button not found');
      record('T7-T12', false, 'Settings button not found');
    }

    // ── T13: Memory sidebar visible ──────────────────────────────────────
    console.log('─── Screen: Memory Sidebar ───');
    const sidebar = page.locator('.memory-sidebar, aside, [class*="sidebar"], [class*="memory"]').first();
    const sidebarVisible = await sidebar.isVisible({ timeout: 2000 }).catch(() => false);
    record('T13: Memory sidebar visible', sidebarVisible);

    // ── T14: Memory entries rendered ─────────────────────────────────────
    if (sidebarVisible) {
      const memoryItems = page.locator('.memory-list li, .memory-card, .memory-item');
      const memCount = await memoryItems.count();
      record('T14: Memory entries rendered', memCount > 0, `${memCount} items`);
    } else {
      record('T14: Memory entries rendered', false, 'Sidebar not visible');
    }

    // ── T15: No uncaught JS errors during test ──────────────────────────────
    console.log('─── Summary ───');
    const criticalErrors = consoleErrors.filter(e =>
      !e.includes('[MOCK]') && !e.includes('invoke') && !e.includes('__TAURI__')
    );
    record('T15: No uncaught JS errors during test', criticalErrors.length === 0,
      criticalErrors.length > 0 ? `${criticalErrors.length} errors: ${criticalErrors.join(' | ')}` : 'clean');

    // ── T16: Wizard renders for fresh user ────────────────────────────────
    console.log('─── Screen: Wizard (fresh user) ───');
    const wizardContext = await browser.newContext({ viewport: { width: 1200, height: 800 } });
    await wizardContext.addInitScript(() => {
      localStorage.removeItem('wizard_completed');
      window.__TAURI__ = {
        core: {
          invoke: async (cmd) => {
            const mocks = {
              get_settings: { providers: [], routing: {}, mcpServers: [] },
              check_setup: { needs_setup: true },
              get_memories: [],
              get_praxis_guidance: [],
              get_analysis_events: [],
            };
            return mocks[cmd] ?? null;
          },
        },
        event: { listen: async () => () => {}, emit: async () => {} },
      };
    });
    const wizPage = await wizardContext.newPage();
    await wizPage.goto(`http://localhost:${PORT}`, { waitUntil: 'networkidle' });
    await wizPage.waitForTimeout(1000);
    const wizardOverlay = wizPage.locator('.wizard-overlay, [aria-label*="Wizard" i], [aria-label*="Setup" i]').first();
    const wizardVisible = await wizardOverlay.isVisible({ timeout: 3000 }).catch(() => false);
    record('T16: Wizard shows for fresh user', wizardVisible);

    // ── T17: Wizard has input for agent name ──────────────────────────────
    if (wizardVisible) {
      const nameInput = wizPage.locator('input[placeholder*="name" i], input[type="text"]').first();
      const nameVisible = await nameInput.isVisible({ timeout: 2000 }).catch(() => false);
      record('T17: Wizard has name input', nameVisible);
    } else {
      record('T17: Wizard has name input', false, 'Wizard not visible');
    }
    await wizardContext.close();

  } finally {
    await browser.close();
    server.close();
  }

  // ── Report ─────────────────────────────────────────────────────────────
  const passed = RESULTS.filter(r => r.pass).length;
  const failed = RESULTS.filter(r => !r.pass).length;
  const total = RESULTS.length;
  const passRate = Math.round((passed / total) * 100);

  console.log(`\n${'═'.repeat(50)}`);
  console.log(`QA Results: ${passed}/${total} passed (${passRate}%)`);
  if (failed > 0) {
    console.log(`\nFailed tests:`);
    RESULTS.filter(r => !r.pass).forEach(r => console.log(`  ❌ ${r.name}: ${r.detail}`));
  }
  console.log(`${'═'.repeat(50)}\n`);

  // Exit with non-zero if any failures
  process.exit(failed > 0 ? 1 : 0);
}

runQA().catch((err) => {
  console.error('QA suite crashed:', err);
  process.exit(1);
});
