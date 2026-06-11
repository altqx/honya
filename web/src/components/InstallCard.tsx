import { useEffect, useRef, useState } from 'react'
import { CheckIcon, CopyIcon } from './icons'
import { INSTALL_CARGO, INSTALL_PS, INSTALL_SH } from '../data/site'

async function copyText(text: string) {
  if (
    typeof navigator !== 'undefined' &&
    navigator.clipboard?.writeText &&
    window.isSecureContext
  ) {
    try {
      await navigator.clipboard.writeText(text)
      return true
    } catch {
      /* fall through to legacy path */
    }
  }
  try {
    const ta = document.createElement('textarea')
    ta.value = text
    ta.setAttribute('readonly', '')
    ta.style.position = 'fixed'
    ta.style.top = '-1000px'
    ta.style.opacity = '0'
    document.body.appendChild(ta)
    ta.focus()
    ta.select()
    const ok = document.execCommand('copy')
    document.body.removeChild(ta)
    return ok
  } catch {
    return false
  }
}

function detectWindows() {
  if (typeof navigator === 'undefined') return false
  let uaPlat = ''
  try {
    uaPlat =
      (navigator as unknown as { userAgentData?: { platform?: string } })
        .userAgentData?.platform ?? ''
  } catch {
    uaPlat = ''
  }
  const probe = (
    uaPlat +
    ' ' +
    (navigator.platform || '') +
    ' ' +
    (navigator.userAgent || '')
  ).toLowerCase()
  return probe.indexOf('win') !== -1 && probe.indexOf('windows phone') === -1
}

function AltPill({
  label,
  command,
  ariaBase,
}: {
  label: string
  command: string
  ariaBase: string
}) {
  const [copied, setCopied] = useState(false)
  const timer = useRef<number | undefined>(undefined)
  const onCopy = async () => {
    const ok = await copyText(command)
    if (!ok) return
    setCopied(true)
    window.clearTimeout(timer.current)
    timer.current = window.setTimeout(() => setCopied(false), 2200)
  }
  return (
    <div className="alt-install">
      <span className="or">{label}</span>
      <span className="alt-pill">
        <code>{command}</code>
        <button
          className={`alt-copy${copied ? ' copied' : ''}`}
          type="button"
          aria-label={`คัดลอก${ariaBase}ไปยังคลิปบอร์ด`}
          onClick={onCopy}
        >
          <CopyIcon className="ico-copy" />
          <CheckIcon className="ico-check" />
        </button>
      </span>
    </div>
  )
}

export function InstallCard() {
  const [isWin, setIsWin] = useState(false)
  const [copied, setCopied] = useState(false)
  const [status, setStatus] = useState<{ text: string; ok: boolean } | null>(null)
  const timer = useRef<number | undefined>(undefined)

  useEffect(() => {
    setIsWin(detectWindows())
  }, [])

  // On Windows promote the PowerShell one-liner; the shell installer (Linux/macOS)
  // becomes the secondary row, so nothing is shown twice.
  const mainCmd = isWin ? INSTALL_PS : INSTALL_SH
  const mainPrompt = isWin ? 'PS> ' : '$ '
  const altShellLabel = isWin ? 'Linux / macOS' : 'Windows (PowerShell)'
  const altShellCmd = isWin ? INSTALL_SH : INSTALL_PS
  const altShellAria = isWin
    ? 'คำสั่งติดตั้งสำหรับ Linux / macOS'
    : 'คำสั่งติดตั้งสำหรับ Windows'

  const onCopyMain = async () => {
    const ok = await copyText(mainCmd)
    window.clearTimeout(timer.current)
    setCopied(ok)
    setStatus(
      ok
        ? {
            text: 'คัดลอกไปยังคลิปบอร์ดแล้ว — วางลงในเทอร์มินัลของคุณได้เลย',
            ok: true,
          }
        : { text: 'คัดลอกอัตโนมัติไม่สำเร็จ — กรุณาเลือกคำสั่งแล้วคัดลอกเอง', ok: false },
    )
    timer.current = window.setTimeout(() => {
      setCopied(false)
      setStatus(null)
    }, 2600)
  }

  return (
    <div className="install-card">
      <div className="sec-head" style={{ textAlign: 'center' }}>
        <span className="eyebrow" style={{ justifyContent: 'center' }}>
          ติดตั้ง
        </span>
        <h2 id="install-h">ติดตั้งจากสคริปต์หรือ cargo</h2>
        <p style={{ marginInline: 'auto' }}>
          สคริปต์จะดาวน์โหลดไบนารีจาก GitHub Release เปิดครั้งแรกค่อยวางคีย์สำหรับ
          API ที่รองรับ OpenRouter
        </p>
      </div>

      <div className="copybox-wrap">
        <div className="copybox">
          <code className="cb-cmd">
            <span className="cb-prompt" aria-hidden="true">
              {mainPrompt}
            </span>
            {isWin ? (
              <>
                irm {`https://honya.altqx.com/install.ps1`}{' '}
                <span className="cb-pipe">|</span> iex
              </>
            ) : (
              <>
                curl -fsSL {`https://honya.altqx.com/install.sh`}{' '}
                <span className="cb-pipe">|</span> bash
              </>
            )}
          </code>
          <button
            className={`copy-btn${copied ? ' copied' : ''}`}
            type="button"
            aria-label="คัดลอกคำสั่งติดตั้งไปยังคลิปบอร์ด"
            onClick={onCopyMain}
          >
            <CopyIcon className="ico-copy" />
            <CheckIcon className="ico-check" />
            <span className="copy-label">{copied ? 'คัดลอกแล้ว' : 'คัดลอก'}</span>
          </button>
        </div>
        <p
          className={`copy-status${status ? ' show' : ''}`}
          role="status"
          aria-live="polite"
          style={status && !status.ok ? { color: 'var(--vermilion)' } : undefined}
        >
          {status?.text ?? ''}
        </p>

        <AltPill
          label="หรือด้วย cargo"
          command={INSTALL_CARGO}
          ariaBase="คำสั่งติดตั้งด้วย cargo"
        />
        <AltPill label={altShellLabel} command={altShellCmd} ariaBase={altShellAria} />

        <p className="install-update">
          ติดตั้งไว้แล้ว? อัปเดตได้ทันทีด้วย <code>honya update</code>
        </p>

        <div className="install-foot">
          <span>
            <CheckIcon />
            ทำงานกับ API ที่รองรับ OpenRouter
          </span>
          <span>
            <CheckIcon />
            ขอคีย์ของคุณเมื่อรันครั้งแรก
          </span>
          <span>
            <CheckIcon />
            โปรเจกต์เก็บบนดิสก์
          </span>
        </div>
      </div>
    </div>
  )
}
