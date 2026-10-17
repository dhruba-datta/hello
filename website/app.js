// Hello website: small interactions. No dependencies.
(() => {
  const root = document.documentElement;
  root.classList.replace("no-js", "js");

  const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const $ = (sel, scope = document) => scope.querySelector(sel);
  const $$ = (sel, scope = document) => Array.from(scope.querySelectorAll(sel));
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

  // Header: a border appears once the page scrolls.
  const header = $(".site-header");
  const onScroll = () => header.classList.toggle("scrolled", window.scrollY > 8);
  onScroll();
  window.addEventListener("scroll", onScroll, { passive: true });

  // Mobile menu.
  const toggle = $(".nav-toggle");
  const menu = $("#site-menu");
  const setMenu = (open, { restoreFocus = false } = {}) => {
    toggle.setAttribute("aria-expanded", String(open));
    menu.hidden = !open;
    if (!open && restoreFocus) toggle.focus();
  };
  toggle.addEventListener("click", () => setMenu(menu.hidden));
  $$("a", menu).forEach((link) => link.addEventListener("click", () => setMenu(false)));
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !menu.hidden) setMenu(false, { restoreFocus: true });
  });
  window.matchMedia("(min-width: 901px)").addEventListener("change", (e) => {
    if (e.matches) setMenu(false);
  });

  // Active section in the top navigation.
  const navLinks = $$(".nav-links a");
  const linkFor = new Map(navLinks.map((a) => [a.getAttribute("href").slice(1), a]));
  // Sections that share a link. "download" clears the highlight.
  const sectionLink = { how: "features", features: "features", bangla: "bangla", notetaker: "notetaker", privacy: "privacy", setup: "setup", faq: "faq", download: null };
  const setActive = (key) => {
    navLinks.forEach((a) => a.removeAttribute("aria-current"));
    const link = key ? linkFor.get(key) : null;
    if (link) link.setAttribute("aria-current", "true");
  };
  if ("IntersectionObserver" in window) {
    const spy = new IntersectionObserver((entries) => {
      entries.forEach((entry) => {
        if (entry.isIntersecting) setActive(sectionLink[entry.target.id] ?? null);
      });
    }, { rootMargin: "-40% 0px -55% 0px" });
    Object.keys(sectionLink).forEach((id) => {
      const el = document.getElementById(id);
      if (el) spy.observe(el);
    });
  }

  // Reveal sections as they scroll into view.
  const reveals = $$(".reveal");
  if (reduceMotion || !("IntersectionObserver" in window)) {
    reveals.forEach((el) => el.classList.add("in"));
  } else {
    const io = new IntersectionObserver((entries, observer) => {
      entries.forEach((entry) => {
        if (entry.isIntersecting) {
          entry.target.classList.add("in");
          observer.unobserve(entry.target);
        }
      });
    }, { rootMargin: "0px 0px -6% 0px", threshold: 0.05 });
    reveals.forEach((el) => io.observe(el));
  }

  // Tabs: arrow keys move between tabs, as the ARIA pattern expects.
  const bindTabs = (tabs, select) => {
    tabs.forEach((tab, index) => {
      tab.addEventListener("click", () => select(tab));
      tab.addEventListener("keydown", (e) => {
        const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
        if (!step) return;
        e.preventDefault();
        const next = tabs[(index + step + tabs.length) % tabs.length];
        select(next);
        next.focus();
      });
    });
  };

  // Hero demo: listens, processes, then types the dictation into a window. Decorative.
  const demo = {
    app: $("#demo-app"),
    lang: $("#demo-lang"),
    text: $("#demo-text"),
    hint: $("#demo-hint"),
    indicator: $("#demo-indicator"),
  };
  const examples = [
    { app: "Notes", lang: "English", cls: "", parts: [["Remind me to send the launch notes to the team before Friday.", "en"]] },
    { app: "Team chat", lang: "Bangla + English", cls: "mix", parts: [["আজকের ", "bn"], ["meeting", "en"], [" টা বিকেল ৩টায়, ", "bn"], ["please", "en"], [" সবাইকে জানিয়ে দিও।", "bn"]] },
    { app: "Chat", lang: "English + Bangla", cls: "mix", parts: [["Running a bit late, ", "en"], ["আমি দশ মিনিটের মধ্যে আসছি।", "bn"]] },
    { app: "Email", lang: "Bangla", cls: "bn", parts: [["ধন্যবাদ, আমি কালকের মধ্যে রিপোর্টটা পাঠিয়ে দেব।", "bn"]] },
  ];

  const renderStatic = (ex) => {
    demo.app.textContent = ex.app;
    demo.lang.textContent = ex.lang;
    demo.lang.className = `demo-lang ${ex.cls}`;
    demo.text.innerHTML = "";
    ex.parts.forEach(([t, lang]) => {
      const span = document.createElement("span");
      if (lang === "bn") span.className = "bn-run";
      span.textContent = t;
      demo.text.appendChild(span);
    });
  };

  const runDemo = async () => {
    if (reduceMotion) { renderStatic(examples[1]); demo.indicator.classList.add("on"); return; }
    const seg = "Segmenter" in Intl ? new Intl.Segmenter(undefined, { granularity: "grapheme" }) : null;
    const caret = document.createElement("span");
    caret.className = "caret";
    await sleep(700);
    for (let i = 0; ; i++) {
      const ex = examples[i % examples.length];
      demo.app.textContent = ex.app;
      demo.lang.textContent = ex.lang;
      demo.lang.className = `demo-lang ${ex.cls}`;
      demo.text.innerHTML = "";
      demo.text.appendChild(caret);

      // Listening.
      demo.hint.classList.remove("off");
      demo.indicator.classList.remove("processing");
      demo.indicator.classList.add("on");
      await sleep(2000);
      // Processing.
      demo.hint.classList.add("off");
      demo.indicator.classList.add("processing");
      await sleep(650);
      demo.indicator.classList.remove("on", "processing");

      // Type it out. Bangla characters are typed as whole clusters.
      for (const [t, lang] of ex.parts) {
        const span = document.createElement("span");
        if (lang === "bn") span.className = "bn-run";
        demo.text.insertBefore(span, caret);
        const chars = seg ? Array.from(seg.segment(t), (s) => s.segment) : Array.from(t);
        for (const ch of chars) {
          span.textContent += ch;
          await sleep(ch === " " ? 18 : 26);
        }
      }
      await sleep(2800);
    }
  };
  runDemo();

  // "Preview the indicator": hold the button, or hold Option, to see the animation. Nothing is recorded.
  const tryBtn = $("#try-btn");
  const tryState = $("#try-state");
  let holding = false;
  let doneTimer = 0;
  const startHold = () => {
    if (holding) return;
    holding = true;
    clearTimeout(doneTimer);
    tryBtn.classList.add("on");
    tryState.textContent = "Previewing…";
  };
  const endHold = () => {
    if (!holding) return;
    holding = false;
    tryBtn.classList.remove("on");
    tryState.textContent = "Preview done";
    doneTimer = setTimeout(() => { if (!holding) tryState.textContent = "Hold to preview"; }, 1400);
  };
  tryBtn.addEventListener("pointerdown", (e) => {
    e.preventDefault();
    tryBtn.setPointerCapture?.(e.pointerId);
    startHold();
  });
  ["pointerup", "pointercancel", "lostpointercapture"].forEach((type) => tryBtn.addEventListener(type, endHold));
  tryBtn.addEventListener("keydown", (e) => {
    if ((e.key === " " || e.key === "Enter") && !e.repeat) { e.preventDefault(); startHold(); }
  });
  tryBtn.addEventListener("keyup", (e) => { if (e.key === " " || e.key === "Enter") endHold(); });
  window.addEventListener("keydown", (e) => {
    if (e.key === "Alt" && !e.repeat && !e.metaKey && !e.ctrlKey && !e.shiftKey) startHold();
  });
  window.addEventListener("keyup", (e) => { if (e.key === "Alt") endHold(); });
  window.addEventListener("blur", endHold);

  // Indicator picker: 17 styles, and a place on the screen (top or bottom, left, center or right).
  const screen = $("#screen");
  const panel = $("#style-panel");
  const specTabs = $$('[role="tab"][data-style]');
  const specimens = $$(".spec-slot .sp");
  const linePath = $("#line-path");

  const selectStyle = (tab) => {
    specTabs.forEach((t) => {
      const on = t === tab;
      t.setAttribute("aria-selected", String(on));
      t.tabIndex = on ? 0 : -1;
    });
    const key = tab.dataset.style;
    screen.dataset.style = key;
    specimens.forEach((sp) => { sp.hidden = sp.dataset.s !== key; });
    panel.setAttribute("aria-labelledby", tab.id);
    $("#style-name").textContent = tab.textContent;
    $("#style-desc").textContent = tab.dataset.desc;
    $("#style-default").hidden = key !== "wave";
    setLineRunning(key === "line" && lineVisible && !reduceMotion);
  };
  bindTabs(specTabs, selectStyle);

  // Position: top or bottom, and left, center or right. The buttons and the six spots on the screen stay in step.
  const verticalButtons = $$(".pos-btn[data-vpos]");
  const sideButtons = $$(".pos-btn[data-side]");
  const spots = $$(".spot");
  const where = { top: "near the menu bar", bottom: "just above the Dock", left: "on the left", center: "in the middle", right: "on the right" };
  let vpos = "bottom";
  let side = "center";
  const setPosition = (next) => {
    if (next.vpos) vpos = next.vpos;
    if (next.side) side = next.side;
    screen.dataset.vpos = vpos;
    screen.dataset.side = side;
    verticalButtons.forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.vpos === vpos)));
    sideButtons.forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.side === side)));
    spots.forEach((s) => s.setAttribute("aria-pressed", String(s.dataset.y === vpos && s.dataset.x === side)));
    $("#pos-note").textContent = `Shown ${where[vpos]}, ${where[side]}.`;
  };
  verticalButtons.forEach((b) => b.addEventListener("click", () => setPosition({ vpos: b.dataset.vpos })));
  sideButtons.forEach((b) => b.addEventListener("click", () => setPosition({ side: b.dataset.side })));
  spots.forEach((s) => s.addEventListener("click", () => setPosition({ vpos: s.dataset.y, side: s.dataset.x })));
  setPosition({});

  // Line style: a sound-wave trace. It animates only while the picker is on screen and Line is selected.
  let lineVisible = false;
  let lineRunning = false;
  let lineLooping = false;
  const renderLine = (t) => {
    let d = "";
    for (let i = 0; i <= 48; i++) {
      const x = i * (68 / 48);
      const dx = x - 34;
      const taper = Math.max(0, 1 - (dx / 30) ** 2);
      const wave = Math.sin(dx * 0.42 - t * 9) * 0.6 + Math.sin(dx * 0.23 + t * 5.3) * 0.3 + Math.sin(dx * 0.71 - t * 13) * 0.1;
      const y = 12 - 8 * taper * wave;
      d += `${i === 0 ? "M" : " L"}${x.toFixed(2)} ${y.toFixed(2)}`;
    }
    linePath.setAttribute("d", d);
  };
  const lineFrame = () => {
    if (!lineRunning) { lineLooping = false; return; }
    renderLine(performance.now() / 1000);
    requestAnimationFrame(lineFrame);
  };
  const setLineRunning = (on) => {
    lineRunning = on;
    if (on && !lineLooping) { lineLooping = true; requestAnimationFrame(lineFrame); }
    if (!on) renderLine(0.7);
  };
  // The specimens animate (CSS, via .is-live) and the Line trace draws, only while the picker is on screen.
  const setLive = (on) => {
    screen.classList.toggle("is-live", on);
    setLineRunning(on && screen.dataset.style === "line");
  };
  if ("IntersectionObserver" in window) {
    new IntersectionObserver(([entry]) => {
      lineVisible = entry.isIntersecting && !reduceMotion;
      setLive(lineVisible);
    }).observe(screen);
  }
  renderLine(0.7);

  // Bangla examples.
  const mixOut = $("#mix-out");
  const mixes = [
    `আজকের <span class="en-word">meeting</span> টা বিকেল ৩টায়, <span class="en-word">please</span> সবাইকে জানিয়ে দিও।`,
    `<span class="en-word">Can you check the report?</span> আমার মনে হয় দ্বিতীয় অংশে একটা ভুল আছে।`,
    `আমি আজকে বাসা থেকে কাজ করছি, কোনো দরকার হলে ফোন দিও।`,
  ];
  const mixTabs = $$('[role="tab"][data-mix]');
  bindTabs(mixTabs, (tab) => {
    mixTabs.forEach((t) => {
      const on = t === tab;
      t.setAttribute("aria-selected", String(on));
      t.tabIndex = on ? 0 : -1;
    });
    mixOut.style.opacity = "0";
    setTimeout(() => {
      mixOut.innerHTML = mixes[Number(tab.dataset.mix)];
      mixOut.style.opacity = "1";
    }, reduceMotion ? 0 : 200);
  });

  $("#year").textContent = String(new Date().getFullYear());
})();
