// Ready-to-copy client configuration for the Agents page. Pure functions, no DOM:
//   TABS[i].blocks(v) -> [{ kind:'code', title, text } | { kind:'kv', title, rows:[{ k, v, secret? }] }]
// v = { base, anth, key, model }. `base` ends in /v1 (OpenAI style), `anth` is the Anthropic
// root without /v1, `model` may be ''. The caller decides whether `key` is a mask, a
// placeholder or the real secret, so the same templates produce the preview and the clipboard text.

const js = (s) => JSON.stringify(String(s));                                  // JS / JSON / Python / YAML string literal
const sh = (s) => "'" + String(s).replace(/'/g, "'\\''") + "'";               // POSIX single quotes
const shu = (s) => (/^[\w@%+=:,./-]+$/.test(s) ? s : sh(s));                  // quote only when needed
const ps = (s) => "'" + String(s).replace(/'/g, "''") + "'";                  // PowerShell single quotes

const modelOf = (v) => v.model || 'MODEL_ID';
const code = (title, text) => ({ kind: 'code', title, text });

function chatBody(v) {
  return JSON.stringify({ model: modelOf(v), messages: [{ role: 'user', content: 'Hello' }] });
}

function curl(v) {
  const anthBody = JSON.stringify({ model: modelOf(v), max_tokens: 1024, messages: [{ role: 'user', content: 'Hello' }] });
  return [
    code('Chat Completions (OpenAI)', [
      `curl ${shu(v.base + '/chat/completions')} \\`,
      `  -H ${sh('Authorization: Bearer ' + v.key)} \\`,
      `  -H 'Content-Type: application/json' \\`,
      `  -d ${sh(chatBody(v))}`,
    ].join('\n')),
    code('Models', `curl ${shu(v.base + '/models')} \\\n  -H ${sh('Authorization: Bearer ' + v.key)}`),
    code('Messages (Anthropic)', [
      `curl ${shu(v.anth + '/v1/messages')} \\`,
      `  -H ${sh('x-api-key: ' + v.key)} \\`,
      `  -H 'anthropic-version: 2023-06-01' \\`,
      `  -H 'Content-Type: application/json' \\`,
      `  -d ${sh(anthBody)}`,
    ].join('\n')),
  ];
}

function openai(v) {
  return [
    code('Python', [
      'from openai import OpenAI', '',
      'client = OpenAI(', `    base_url=${js(v.base)},`, `    api_key=${js(v.key)},`, ')', '',
      'resp = client.chat.completions.create(', `    model=${js(modelOf(v))},`,
      '    messages=[{"role": "user", "content": "Hello"}],', ')',
      'print(resp.choices[0].message.content)',
    ].join('\n')),
    code('Node.js', [
      'import OpenAI from "openai";', '',
      'const client = new OpenAI({', `  baseURL: ${js(v.base)},`, `  apiKey: ${js(v.key)},`, '});', '',
      'const resp = await client.chat.completions.create({', `  model: ${js(modelOf(v))},`,
      '  messages: [{ role: "user", content: "Hello" }],', '});',
      'console.log(resp.choices[0].message.content);',
    ].join('\n')),
  ];
}

function anthropic(v) {
  return [
    code('Python', [
      'import anthropic', '',
      'client = anthropic.Anthropic(', `    base_url=${js(v.anth)},`, `    api_key=${js(v.key)},`, ')', '',
      'msg = client.messages.create(', `    model=${js(modelOf(v))},`, '    max_tokens=1024,',
      '    messages=[{"role": "user", "content": "Hello"}],', ')',
      'print(msg.content[0].text)',
    ].join('\n')),
    code('Node.js', [
      'import Anthropic from "@anthropic-ai/sdk";', '',
      'const client = new Anthropic({', `  baseURL: ${js(v.anth)},`, `  apiKey: ${js(v.key)},`, '});', '',
      'const msg = await client.messages.create({', `  model: ${js(modelOf(v))},`, '  max_tokens: 1024,',
      '  messages: [{ role: "user", content: "Hello" }],', '});',
      'console.log(msg.content[0].text);',
    ].join('\n')),
  ];
}

function claude(v) {
  const env = { ANTHROPIC_BASE_URL: v.anth, ANTHROPIC_AUTH_TOKEN: v.key };
  if (v.model) env.ANTHROPIC_MODEL = v.model;
  return [
    code('macOS / Linux', [...Object.entries(env).map(([k, x]) => `export ${k}=${sh(x)}`), 'claude'].join('\n')),
    code('Windows PowerShell', [...Object.entries(env).map(([k, x]) => `$env:${k} = ${ps(x)}`), 'claude'].join('\n')),
    code('~/.claude/settings.json', JSON.stringify({ env }, null, 2)),
  ];
}

function editors(v) {
  return [
    {
      kind: 'kv', title: 'Cursor / Cline / Roo Code',
      rows: [
        { k: 'API Provider', v: 'OpenAI Compatible' },
        { k: 'Base URL', v: v.base },
        { k: 'API Key', v: v.key, secret: true },
        { k: 'Model ID', v: modelOf(v) },
      ],
    },
    code('Continue (config.yaml)', [
      'name: WorkBuddy Hub', 'version: 1.0.0', 'schema: v1', 'models:', '  - name: WorkBuddy Hub', '    provider: openai',
      `    model: ${js(modelOf(v))}`, `    apiBase: ${js(v.base)}`, `    apiKey: ${js(v.key)}`, '    roles:', '      - chat', '      - edit',
    ].join('\n')),
  ];
}

function env(v) {
  const pairs = [['OPENAI_BASE_URL', v.base], ['OPENAI_API_BASE', v.base], ['OPENAI_API_KEY', v.key], ['ANTHROPIC_BASE_URL', v.anth], ['ANTHROPIC_AUTH_TOKEN', v.key]];
  if (v.model) pairs.push(['OPENAI_MODEL', v.model], ['ANTHROPIC_MODEL', v.model]);
  return [
    code('macOS / Linux', pairs.map(([k, x]) => `export ${k}=${sh(x)}`).join('\n')),
    code('Windows PowerShell', pairs.map(([k, x]) => `$env:${k} = ${ps(x)}`).join('\n')),
    code('.env', pairs.map(([k, x]) => `${k}=${js(x)}`).join('\n')),
  ];
}

/** Tab labels are brand names shown as is; the page supplies the translated label for the empty one. */
export const TABS = [
  { id: 'curl', label: 'curl', blocks: curl },
  { id: 'openai', label: 'OpenAI SDK', blocks: openai },
  { id: 'anthropic', label: 'Anthropic SDK', blocks: anthropic, needsAnthropic: true },
  { id: 'claude', label: 'Claude Code', blocks: claude, needsAnthropic: true },
  { id: 'editors', label: '', blocks: editors },
  { id: 'env', label: '', blocks: env },
];

/** Plain text of one block, as copied to the clipboard. */
export function blockText(b) {
  return b.kind === 'kv' ? b.rows.map((r) => `${r.k}: ${r.v}`).join('\n') : b.text;
}
