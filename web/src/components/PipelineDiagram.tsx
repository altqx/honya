export function PipelineDiagram() {
  return (
    <div className="pipe-svg-card">
      <svg
        className="pipe-svg"
        viewBox="0 0 920 280"
        role="img"
        aria-labelledby="pipe-svg-title pipe-svg-desc"
        preserveAspectRatio="xMidYMid meet"
      >
        <title id="pipe-svg-title">แผนภาพไปป์ไลน์สามเอเจนต์ของ honya</title>
        <desc id="pipe-svg-desc">
          Chunk และบริบทไหลเข้าสู่ Orchestrator ซึ่งเลือกศัพท์ ตัวละคร โปรเจกต์
          และสไตล์ที่เกี่ยวข้อง จากนั้น Translator ร่างภาษาไทย ส่วน Reviewer
          จะอนุมัติให้แอปผนวกคำแปล หรือปฏิเสธพร้อมฟีดแบ็กที่ส่งกลับไปให้ Translator
          ลองใหม่
        </desc>
        <defs>
          <marker
            id="arrow"
            markerWidth="9"
            markerHeight="9"
            refX="6.5"
            refY="4.5"
            orient="auto"
          >
            <path d="M0 0 L9 4.5 L0 9 Z" fill="#5C564E" />
          </marker>
          <marker
            id="arrow-v"
            markerWidth="9"
            markerHeight="9"
            refX="6.5"
            refY="4.5"
            orient="auto"
          >
            <path d="M0 0 L9 4.5 L0 9 Z" fill="#B24A3A" />
          </marker>
          <marker
            id="arrow-i"
            markerWidth="9"
            markerHeight="9"
            refX="6.5"
            refY="4.5"
            orient="auto"
          >
            <path d="M0 0 L9 4.5 L0 9 Z" fill="#3A5078" />
          </marker>
        </defs>

        <rect x="8" y="108" width="118" height="64" rx="10" fill="#E5DFD2" stroke="#CEC6B8" />
        <text x="67" y="134" textAnchor="middle" fontFamily="JetBrains Mono, monospace" fontSize="12.5" fill="#2D2A26">
          Chunk
        </text>
        <text x="67" y="152" textAnchor="middle" fontFamily="JetBrains Mono, monospace" fontSize="10.5" fill="#968E82">
          ~1000 โทเคน
        </text>

        <rect x="8" y="200" width="118" height="56" rx="10" fill="#DEE0E8" stroke="#6C80A2" />
        <text x="67" y="222" textAnchor="middle" fontFamily="JetBrains Mono, monospace" fontSize="10.5" fill="#3A5078">
          ศัพท์·ตัวละคร
        </text>
        <text x="67" y="238" textAnchor="middle" fontFamily="JetBrains Mono, monospace" fontSize="10.5" fill="#3A5078">
          โปรเจกต์·สไตล์
        </text>

        <text x="67" y="86" textAnchor="middle" fontFamily="JetBrains Mono, monospace" fontSize="9.5" fill="#968E82">
          + 5 บรรทัด TH ก่อนหน้า
        </text>

        <line x1="126" y1="140" x2="186" y2="140" stroke="#5C564E" strokeWidth="1.6" markerEnd="url(#arrow)" />
        <line x1="126" y1="222" x2="160" y2="222" stroke="#6C80A2" strokeWidth="1.6" />
        <line x1="160" y1="222" x2="160" y2="158" stroke="#6C80A2" strokeWidth="1.6" markerEnd="url(#arrow-i)" />

        <rect x="188" y="98" width="180" height="84" rx="12" fill="#F3EFE6" stroke="#3A5078" strokeWidth="1.6" />
        <text x="278" y="128" textAnchor="middle" fontFamily="Noto Serif JP, serif" fontSize="17" fill="#2D2A26">
          指揮 Orchestrator
        </text>
        <text x="278" y="150" textAnchor="middle" fontFamily="Zen Kaku Gothic New, sans-serif" fontSize="11" fill="#5C564E">
          รวบรวมบริบท
        </text>
        <text x="278" y="166" textAnchor="middle" fontFamily="Zen Kaku Gothic New, sans-serif" fontSize="11" fill="#5C564E">
          บันทึกสถานะ · สรุป
        </text>

        <line x1="368" y1="140" x2="428" y2="140" stroke="#5C564E" strokeWidth="1.6" markerEnd="url(#arrow)" />

        <rect x="430" y="98" width="170" height="84" rx="12" fill="#F3EFE6" stroke="#3A5078" strokeWidth="1.6" />
        <text x="515" y="128" textAnchor="middle" fontFamily="Noto Serif JP, serif" fontSize="17" fill="#2D2A26">
          訳者 Translator
        </text>
        <text x="515" y="150" textAnchor="middle" fontFamily="Zen Kaku Gothic New, sans-serif" fontSize="11" fill="#5C564E">
          ร่างภาษาไทย
        </text>
        <text x="515" y="166" textAnchor="middle" fontFamily="Zen Kaku Gothic New, sans-serif" fontSize="11" fill="#5C564E">
          สตรีมโทเคน
        </text>

        <line x1="600" y1="140" x2="660" y2="140" stroke="#5C564E" strokeWidth="1.6" markerEnd="url(#arrow)" />

        <rect x="662" y="98" width="170" height="84" rx="12" fill="#F3EFE6" stroke="#3A5078" strokeWidth="1.6" />
        <text x="747" y="128" textAnchor="middle" fontFamily="Noto Serif JP, serif" fontSize="17" fill="#2D2A26">
          校正 Reviewer
        </text>
        <text x="747" y="150" textAnchor="middle" fontFamily="Zen Kaku Gothic New, sans-serif" fontSize="11" fill="#5C564E">
          ตรวจความตรงต้นฉบับ
        </text>
        <text x="747" y="166" textAnchor="middle" fontFamily="Zen Kaku Gothic New, sans-serif" fontSize="11" fill="#5C564E">
          อนุมัติ / ปฏิเสธ
        </text>

        <line x1="832" y1="140" x2="876" y2="140" stroke="#6A8258" strokeWidth="1.8" markerEnd="url(#arrow)" />
        <text x="888" y="130" textAnchor="middle" fontFamily="JetBrains Mono, monospace" fontSize="11" fill="#6A8258">
          ●
        </text>
        <text x="888" y="156" textAnchor="middle" fontFamily="JetBrains Mono, monospace" fontSize="9.5" fill="#6A8258">
          ผนวก
        </text>

        <path d="M747 182 L747 232 L515 232 L515 184" fill="none" stroke="#B24A3A" strokeWidth="1.6" strokeDasharray="5 4" markerEnd="url(#arrow-v)" />
        <text x="631" y="226" textAnchor="middle" fontFamily="JetBrains Mono, monospace" fontSize="10.5" fill="#B24A3A">
          ปฏิเสธ › ฟีดแบ็กแยกข้อ › ลองใหม่
        </text>

        <path d="M747 98 L747 64 L278 64 L278 96" fill="none" stroke="#6C80A2" strokeWidth="1.4" strokeDasharray="3 4" markerEnd="url(#arrow-i)" />
        <text x="512" y="56" textAnchor="middle" fontFamily="JetBrains Mono, monospace" fontSize="10" fill="#6C80A2">
          อนุมัติ › บันทึกศัพท์ / ตัวละคร / โน้ตใหม่
        </text>
      </svg>
    </div>
  )
}
