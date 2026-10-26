// Drive the Tauri window over the WebView2 debug port.
// Node 22+ ships a global WebSocket, so no dependencies are needed.
const fs = require('node:fs')

const PORT = process.argv[2] ?? '9224'
const OUT_DIR = process.argv[3] ?? '.'

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

async function connect() {
  for (let attempt = 0; attempt < 40; attempt++) {
    try {
      const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json()
      const page = targets.find((t) => t.type === 'page')
      if (page?.webSocketDebuggerUrl) return page
    } catch {
      /* not up yet */
    }
    await sleep(500)
  }
  throw new Error('webview debug endpoint never appeared')
}

async function main() {
  const page = await connect()
  console.log(`page   ${page.title} · ${page.url}`)

  const ws = new WebSocket(page.webSocketDebuggerUrl)
  await new Promise((resolve, reject) => {
    ws.onopen = resolve
    ws.onerror = reject
  })

  let nextId = 1
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = nextId++
      const onMessage = (event) => {
        const message = JSON.parse(event.data)
        if (message.id !== id) return
        ws.removeEventListener('message', onMessage)
        if (message.error) reject(new Error(JSON.stringify(message.error)))
        else resolve(message.result)
      }
      ws.addEventListener('message', onMessage)
      ws.send(JSON.stringify({ id, method, params }))
    })

  const evaluate = async (expression) => {
    const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
    return result.result?.value
  }

  const capture = async (name) => {
    const shot = await send('Page.captureScreenshot', { format: 'png' })
    const file = `${OUT_DIR}\\${name}.png`
    fs.writeFileSync(file, Buffer.from(shot.data, 'base64'))
    console.log(`wrote  ${file} (${Math.round(fs.statSync(file).size / 1024)} KB)`)
  }

  // Idle state.
  await sleep(700)
  console.log(`idle   ${await evaluate('document.getElementById("subtitle").textContent')}`)
  await capture('shot-tauri-idle')

  // Drive a real scan through the UI, not through the backend.
  await evaluate('document.getElementById("scan").click()')
  console.log('scan   started via the window')

  for (let i = 0; i < 60; i++) {
    await sleep(5000)
    const subtitle = await evaluate('document.getElementById("subtitle").textContent')
    if (!subtitle.includes('nothing scanned')) {
      console.log(`done   ${subtitle}`)
      break
    }
  }

  const rows = await evaluate('document.querySelectorAll("#list .row").length')
  const summary = await evaluate('document.getElementById("summary").innerText.replace(/\\n/g, " · ")')
  console.log(`rows   ${rows}`)
  console.log(`stats  ${summary}`)

  await capture('shot-tauri-scanned')
  ws.close()
}

main().catch((error) => {
  console.error('FAILED:', error.message)
  process.exit(1)
})
