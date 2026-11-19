// The PDF layout. Data comes from render.rs as /data.json; every value in it is
// plain text, so nothing a video says can turn into Typst code.
#let d = json("/data.json")
#let t = d.t

#let ink = rgb("#1d2433")
#let muted = rgb("#667085")
#let rule = rgb("#e4e7ec")
#let accent = rgb("#c4302b")
#let soft = rgb("#f6f7f9")

#set document(title: d.title)
#set page(paper: "a4", margin: (x: 16mm, y: 18mm))
#set text(font: d.fonts, size: 10.5pt, fill: ink, lang: d.lang)
#set par(leading: 0.75em, spacing: 1.1em)
#set list(indent: 1mm, body-indent: 2.5mm, spacing: 0.7em)
// Korean breaks lines between words, not between syllables (CSS `word-break: keep-all`).
#show regex("[\u{AC00}-\u{D7A3}]+"): box

// A time that opens that moment of the video, or plain text when there is no video page.
#let at(seconds, label, ..style) = {
  let label = text(fill: accent, number-type: "lining", number-width: "tabular", ..style, label)
  if d.video_url == none { label } else { link(d.video_url + "&t=" + str(seconds) + "s", label) }
}

#let section-title(title) = block(
  width: 100%,
  stroke: (bottom: 2pt + ink),
  inset: (bottom: 2mm),
  below: 5mm,
  heading(level: 1, text(15pt, title)),
)

#show heading: set block(above: 0pt, below: 0pt)
#show heading: set text(weight: 700)

// Cover
#page(margin: (x: 16mm, y: 18mm), align(horizon, {
  text(fill: accent, weight: 700, tracking: 0.08em, size: 9pt, if d.video_url == none { t.kicker_local } else { t.kicker })
  v(6mm)
  par(leading: 0.45em, text(26pt, weight: 700, d.title))
  v(8mm)
  block(stroke: (left: 3pt + accent), inset: (left: 5mm, y: 1mm), text(13pt, d.one_line))
  v(14mm)
  grid(
    columns: (30mm, 1fr),
    column-gutter: 4mm,
    row-gutter: 3mm,
    ..d.details.map(((label, value)) => (text(fill: muted, label), text(10pt, value))).flatten(),
  )
}))

// Overview and contents
#section-title(t.overview)
#for para in d.overview [#para #parbreak()]
#block(
  width: 100%,
  fill: soft,
  radius: 3mm,
  inset: (x: 6mm, y: 4mm),
  above: 4mm,
  below: 8mm,
  list(..d.key_points),
)

#section-title(t.contents)
#for s in d.sections {
  block(
    width: 100%,
    stroke: (bottom: (paint: rule, thickness: 1pt, dash: "dotted")),
    inset: (y: 1.5mm),
    spacing: 0pt,
    grid(columns: (26mm, 1fr), column-gutter: 4mm, at(s.start, s.span), s.title),
  )
}
#v(8mm)

#for c in d.comparisons {
  section-title(t.comparison + ": " + c.title)
  block(below: 8mm, table(
    columns: (auto,) + (1fr,) * c.subjects.len(),
    stroke: (x, y) => (bottom: if y == 0 { 1.5pt + ink } else { 0.5pt + rule }),
    fill: (x, y) => if x == 0 and y > 0 { soft },
    inset: (x: 3mm, y: 2mm),
    table.header(..(([],) + c.subjects).map(s => text(weight: 700, s))),
    ..c.rows.map(r => (text(weight: 700, r.aspect),) + r.values).flatten(),
  ))
}

#if d.concepts.len() > 0 {
  section-title(t.glossary)
  block(below: 8mm, grid(
    columns: (auto, 1fr),
    column-gutter: 5mm,
    row-gutter: 2.5mm,
    ..d.concepts.map(c => (text(weight: 700, fill: accent, c.name), c.meaning)).flatten(),
  ))
}

// Chapters
#pagebreak()
#for (i, s) in d.sections.enumerate() {
  if i > 0 {
    v(9mm)
    line(length: 100%, stroke: 1pt + rule)
    v(7mm)
  }
  block(breakable: false, {
    at(s.start, "▶ " + s.span, weight: 700, size: 10pt)
    v(2.5mm)
    heading(level: 2, text(16pt, str(i + 1) + ". " + s.title))
    v(5mm)
    if s.frames.len() > 0 {
      grid(
        columns: if s.frames.len() == 1 { (1fr,) } else { (1fr, 1fr) },
        gutter: 4mm,
        ..s.frames.map(f => {
          box(radius: 2mm, clip: true, stroke: 1pt + rule, image(f.path, width: 100%))
          v(1mm, weak: true)
          at(f.time.s, f.time.label, size: 8.5pt, fill: muted)
        }),
      )
      v(5mm)
    }
  })
  par(s.summary)
  block(breakable: false, above: 5mm, {
    heading(level: 3, text(10.5pt, fill: muted, t.key_points))
    v(2.5mm)
    list(..s.key_points)
  })
}

// Transcript
#pagebreak()
#section-title(t.transcript)
#set text(9pt)
#for seg in d.segments {
  block(
    width: 100%,
    stroke: (bottom: 1pt + soft),
    inset: (y: 1mm),
    spacing: 0pt,
    grid(columns: (16mm, 1fr), at(seg.time.s, seg.time.label), seg.text),
  )
}
