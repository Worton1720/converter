// Автотесты интерфейса: выполняются в окне отладочной сборки (`tools/ui-test.sh`).
// Проверяют правила для пожилых пользователей (ТЗ 8.5) и сценарии 1–3 (ТЗ, раздел 3).
// Данные теста — `window.__TEST_DATA`: папка с подготовленными файлами.
(async () => {
  const invoke = window.__TAURI_INTERNALS__.invoke;
  const report = (line) => invoke("dev_report", { line });
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const data = window.__TEST_DATA;
  let failed = 0;

  async function until(fn, what, ms = 30000) {
    const end = Date.now() + ms;
    for (;;) {
      const v = await fn();
      if (v) return v;
      if (Date.now() > end) throw new Error(`не дождался: ${typeof what === "function" ? what() : what}`);
      await sleep(200);
    }
  }
  function expect(ok, message) {
    if (!ok) throw new Error(message);
  }
  async function test(name, fn) {
    try {
      await fn();
      await report(`ok   ${name}`);
    } catch (e) {
      failed++;
      await report(`FAIL ${name}: ${e.message}`);
    }
  }

  const visible = (e) => {
    const r = e.getBoundingClientRect();
    const s = getComputedStyle(e);
    return r.width > 0 && r.height > 0 && s.visibility !== "hidden" && s.display !== "none";
  };
  const text = () => document.body.innerText;
  const button = (label) =>
    [...document.querySelectorAll("button")].find((b) => visible(b) && b.textContent.trim().includes(label));
  async function click(label) {
    const b = await until(() => button(label), `кнопка «${label}»`);
    expect(!b.disabled, `кнопка «${label}» недоступна`);
    b.click();
  }

  /** Правила ТЗ 8.5 для того, что сейчас на экране. */
  function checkElderlyRules(screen) {
    const interactive = [
      ...document.querySelectorAll("button, [role=tab], [role=checkbox], [role=switch], [role=combobox], input, a[href]"),
    ].filter(visible);
    const small = interactive.filter((e) => {
      // Маленький значок допустим, если нажимается вся подпись вокруг него.
      const hit = e.closest("label") ?? e;
      return hit.getBoundingClientRect().height < 43.5;
    });
    expect(
      !small.length,
      `${screen}: цели нажатия меньше 44 px: ${small.map((e) => e.textContent.trim() || e.getAttribute("aria-label") || e.tagName).join(", ")}`,
    );
    const unnamed = interactive.filter(
      // Имя для чтения с экрана: текст, aria-label или подпись <label> вокруг.
      (e) =>
        e.tagName === "BUTTON" &&
        !e.textContent.trim() &&
        !e.getAttribute("aria-label") &&
        !e.closest("label")?.textContent.trim(),
    );
    const where = (e) =>
      `${e.getAttribute("role") ?? e.tagName.toLowerCase()} рядом с «${(e.parentElement?.textContent ?? "").trim().slice(0, 40)}»`;
    expect(!unnamed.length, `${screen}: кнопки без подписи: ${unnamed.map(where).join("; ")}`);
    const tiny = new Set();
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const el = n.parentElement;
      if (!n.textContent.trim() || !el || !visible(el)) continue;
      const size = parseFloat(getComputedStyle(el).fontSize);
      if (size < 13.5) tiny.add(`${n.textContent.trim().slice(0, 30)} (${size}px)`);
    }
    expect(!tiny.size, `${screen}: текст мельче 14 px: ${[...tiny].slice(0, 5).join("; ")}`);
  }

  const store = () => window.__store.getState();
  await until(() => window.__store && store().settings && store().info, "запуск программы");

  await test("пустой экран: три шага и правила для пожилых", async () => {
    for (const step of ["Выберите фото и видео", "Что сделать", "Начать"]) {
      expect(text().includes(step), `нет шага «${step}»`);
    }
    checkElderlyRules("пустой экран");
  });

  await test("после сбоя: «Доделать» одной кнопкой", async () => {
    await until(() => text().includes("закрылась посреди работы"), "полоса «закрылась посреди работы»");
    checkElderlyRules("после сбоя");
    const conflictBefore = store().settings.convert.conflict;
    await click("Доделать");
    await until(() => store().order.some((p) => p.endsWith("IMG_C.png")), "файл снова в списке");
    await until(() => !store().run.active && store().rows[store().order.find((p) => p.endsWith("IMG_C.png"))].job.status === "done", "файл доделан", 60000);
    expect(!text().includes("закрылась посреди работы"), "полоса не исчезла");
    expect(store().settings.convert.conflict === conflictBefore, "параметры «доделать» стали настройками");
  });

  await test("сценарий 1: фото с телефона → JPEG", async () => {
    const added = await invoke("add_paths", { paths: [`${data}/phone`] });
    expect(added === 2, `добавлено ${added} файлов вместо 2`);
    await until(() => store().order.filter((p) => p.includes("/phone/")).length === 2, "файлы в списке");
    checkElderlyRules("список файлов");
    await click("Начать");
    const rows = () => store().order.map((p) => store().rows[p]);
    await until(() => rows().every((r) => !["waiting", "running"].includes(r.job.status)), "конец работы", 90000);
    const bad = rows().filter((r) => r.job.status !== "done");
    expect(!bad.length, `не готово: ${bad.map((r) => `${r.item.name}: ${r.job.status} ${r.job.message ?? ""}`).join("; ")}`);
    await until(() => button("Открыть папку с результатом"), "итог с кнопкой «Открыть папку с результатом»");
    checkElderlyRules("итог преобразования");
  });

  await test("сценарий 3: одно фото в разных форматах после преобразования", async () => {
    // Как в ТЗ: из итога преобразования — одной кнопкой к поиску в папке результата.
    await click("Найти одинаковые файлы в этой папке");
    await until(() => text().includes("Где искать одинаковые файлы"), "экран поиска одинаковых файлов");
    expect(store().dupes.roots.some((r) => r.endsWith("/phone")), `папка результата не подставлена: ${store().dupes.roots}`);
    await click("Найти одинаковые файлы");
    await until(
      () => store().dupes.result || store().dupes.notice,
      () => `итог поиска (идёт: ${store().dupes.running}, этап: ${JSON.stringify(store().dupes.progress)})`,
    );
    const found = store().dupes.result?.groups.map((g) => `${g.kind}:${g.files.map((f) => f.path.split("/").pop())}`);
    expect(text().includes("в нескольких"), `нет абзаца про форматы; найдено: ${found}; ${store().dupes.notice ?? ""}`);
    const groups = store().dupes.result.groups.filter((g) => g.kind === "Formats");
    expect(groups.length === 2, `групп форматов ${groups.length} вместо 2`);
    expect(button("Оставить только JPEG"), "нет кнопки «Оставить только JPEG»");
    checkElderlyRules("одно фото в разных форматах");
  });

  await test("сценарий 2: лишняя копия уходит в корзину", async () => {
    store().setDupes((d) => ({ ...d, roots: [`${data}/copies`], result: null, notice: null }));
    await until(() => text().includes("copies") && !text().includes("в нескольких"), "новая папка поиска");
    await click("Найти одинаковые файлы");
    await until(() => text().includes("Нашлось 1 лишняя копия"), "«Нашлось 1 лишняя копия»");
    checkElderlyRules("найдены копии");
    await click("Убрать лишние копии в корзину");
    await until(() => text().includes("Убрано в корзину: 1"), "итог «Убрано в корзину»");
    checkElderlyRules("итог уборки");
    // Передумал — кнопка рядом с итогом.
    await click("Вернуть как было");
    await until(() => text().includes("всё вернулось как было"), "итог «всё вернулось как было»");
  });

  await test("история: что было сделано с файлами", async () => {
    store().setTab("history");
    await until(() => text().includes("Что программа делала с файлами"), "экран истории");
    await until(() => text().includes("Преобразовано 2 файла"), "запись о преобразовании");
    expect(text().includes("Убрано одинаковых файлов: 1"), "нет записи об уборке копии");
    expect(text().includes("Отменено: всё возвращено как было"), "уборка не помечена отменённой");
    expect(button("Открыть папку с результатом"), "нет кнопки «Открыть папку с результатом»");
    checkElderlyRules("история");
    // Отменить преобразование: подтверждение с объяснением, затем результат уходит в корзину.
    await click("Вернуть как было");
    const dialog = await until(() => document.querySelector("[role=dialog]"), "окно подтверждения");
    expect(dialog.innerText.includes("Готовые файлы (2) уйдут в корзину"), `нет объяснения: ${dialog.innerText}`);
    checkElderlyRules("подтверждение отмены");
    [...dialog.querySelectorAll("button")].find((b) => b.textContent.trim() === "Вернуть как было").click();
    await until(() => text().includes("Готово, всё вернулось как было"), "итог отмены преобразования");
  });

  await test("выделение: выбрать нажатием, действия над выбранными", async () => {
    store().setTab("convert");
    await invoke("add_paths", { paths: [`${data}/copies`] });
    await until(() => store().order.length >= 4, "файлы в списке");
    const tile = (name) =>
      [...document.querySelectorAll("[role=button][aria-pressed]")].find((t) => t.getAttribute("aria-label").startsWith(name));
    const first = await until(() => tile("Отпуск.heic"), "картинка «Отпуск.heic»");
    first.click();
    await until(() => text().includes(`Выбрано 1 из ${store().order.length}`), "полоса «Выбрано 1 из …»");
    expect(button("Начать — выбранные (1)"), "кнопка «Начать» не про выбранные");
    expect(text().includes("Будет обработано только выбранные: 1 фото"), "нет пояснения про выбранные");
    checkElderlyRules("выделение");
    // Shift — диапазон от прошлого нажатия.
    const second = tile("Отпуск (копия).heic");
    second.dispatchEvent(new MouseEvent("click", { bubbles: true, shiftKey: true }));
    await until(() => store().selected.size >= 2, "выбор диапазоном");
    await click("Убрать выбранные из списка");
    await until(() => !store().order.some((p) => p.includes("/copies/")), "выбранные убраны из списка");
    expect(store().selected.size === 0, "после удаления осталось выделение");
    const starts = [...document.querySelectorAll("button")].filter(visible).map((b) => b.textContent.trim()).filter((t) => t.startsWith("Начать"));
    expect(!starts.some((t) => t.includes("выбранные")), `осталась кнопка про выбранные: ${JSON.stringify(starts)}`);
  });

  await test("выделение рамкой: мышью, Shift — добавить, Ctrl — убрать, Esc — отменить", async () => {
    store().setTab("convert");
    await invoke("add_paths", { paths: [`${data}/copies`] });
    await until(() => store().order.length >= 4, "файлы в списке");
    store().select([], "set");
    const tiles = await until(() => {
      const t = [...document.querySelectorAll("[role=button][aria-pressed]")];
      return t.length >= 3 && t;
    }, "три картинки на экране");
    const name = (t) => t.getAttribute("aria-label").replace(/, выбрано$/, "");
    const center = (t) => {
      const r = t.getBoundingClientRect();
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
    };
    // Протянуть рамку от `from` до `to` (точки на экране) с клавишами.
    const drag = async (from, to, keys = {}, cancel = false) => {
      const opts = (p) => ({ bubbles: true, clientX: p.x, clientY: p.y, button: 0, pointerId: 1, ...keys });
      document.elementFromPoint(from.x, from.y).dispatchEvent(new PointerEvent("pointerdown", opts(from)));
      for (let k = 1; k <= 5; k++) {
        const p = { x: from.x + ((to.x - from.x) * k) / 5, y: from.y + ((to.y - from.y) * k) / 5 };
        window.dispatchEvent(new PointerEvent("pointermove", opts(p)));
        await sleep(30);
      }
      if (cancel) window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
      else window.dispatchEvent(new PointerEvent("pointerup", opts(to)));
      await sleep(100);
    };
    const sel = () => [...store().selected].map((p) => p.split("/").pop()).sort();
    const [a, b, c] = tiles;
    const ra = a.getBoundingClientRect();
    // Рамка от левого верхнего угла первой картинки до середины второй — выбраны обе.
    await drag({ x: ra.left + 4, y: ra.top + 4 }, center(b));
    expect(sel().length === 2 && sel().includes(name(a)) && sel().includes(name(b)), `рамка: ${sel()}`);
    // С Ctrl — снять первую.
    await drag({ x: ra.left + 4, y: ra.top + 4 }, center(a), { ctrlKey: true });
    expect(sel().length === 1 && sel()[0] === name(b), `Ctrl: ${sel()}`);
    // С Shift — добавить третью.
    const rc = c.getBoundingClientRect();
    await drag({ x: rc.left + 4, y: rc.top + 4 }, center(c), { shiftKey: true });
    expect(sel().length === 2 && sel().includes(name(c)), `Shift: ${sel()}`);
    // Esc посреди рамки — выделение как было.
    await drag({ x: ra.left + 4, y: ra.top + 4 }, center(c), {}, true);
    expect(sel().length === 2 && sel().includes(name(b)) && sel().includes(name(c)), `Esc: ${sel()}`);
    // Рамка не превращается в щелчок: после неё картинка не переключилась.
    expect(text().includes(`Выбрано 2 из ${store().order.length}`), "полоса выделения не совпадает");
    store().select([], "set");
    store().removePaths(store().order.filter((p) => p.includes("/copies/")));
  });

  await test("перенос с телефона: мастер по шагам", async () => {
    store().setTab("convert");
    await click("Перенести с телефона");
    await until(() => text().includes("Можно перенести"), "план переноса");
    const dialog = () => document.querySelector("[role=dialog]");
    // Внутри окна — по точному тексту: «Перенести» есть и в «Перенести с телефона» за окном.
    const press = async (label) => {
      const b = await until(
        () => [...(dialog()?.querySelectorAll("button") ?? [])].find((x) => x.textContent.trim() === label),
        `кнопка «${label}» в окне`,
      );
      b.click();
    };
    expect(dialog().innerText.includes("Тестовый телефон"), "нет имени телефона");
    expect(dialog().innerText.includes("2 фото"), `не то число файлов: ${dialog().innerText}`);
    expect(dialog().innerText.includes("С телефона ничего не удалится"), "нет обещания не удалять");
    checkElderlyRules("мастер: галерея");
    // Куда сохранить — в нижней строке, слева от «Перенести всё».
    const go = [...dialog().querySelectorAll("button")].find((x) => x.textContent.trim() === "Перенести всё");
    const where = [...dialog().querySelectorAll("p")].find((x) => x.textContent.startsWith("Сохранить в папку"));
    expect(go && where && go.parentElement.contains(where), "папка не рядом с кнопкой «Перенести всё»");
    expect(where.getBoundingClientRect().right <= go.getBoundingClientRect().left, "папка не слева от кнопки");
    expect(text().includes("Выбрано 2 из 2"), "по умолчанию выбрано не всё");
    expect(button("Выбрать весь месяц") || button("Снять весь месяц"), "нет выбора месяца");
    // Снять один снимок нажатием — переносится только выбранное.
    const tiles = () => [...dialog().querySelectorAll("[role=button][aria-pressed]")];
    // Снять первый снимок рамкой с Ctrl.
    const r0 = tiles()[0].getBoundingClientRect();
    const o = (x, y) => ({ bubbles: true, clientX: x, clientY: y, button: 0, pointerId: 1, ctrlKey: true });
    document.elementFromPoint(r0.left + 6, r0.top + 6).dispatchEvent(new PointerEvent("pointerdown", o(r0.left + 6, r0.top + 6)));
    for (let k = 1; k <= 4; k++) {
      window.dispatchEvent(new PointerEvent("pointermove", o(r0.left + 6 + k * 15, r0.top + 6 + k * 15)));
      await sleep(30);
    }
    window.dispatchEvent(new PointerEvent("pointerup", o(r0.left + 66, r0.top + 66)));
    // Сразу после рамки щелчок глушится (отпускание — не щелчок по снимку) — как у человека, пауза.
    await sleep(100);
    await until(() => text().includes("Выбрано 1 из 2"), () => `рамка в галерее: ${dialog()?.innerText.slice(0, 200)}`);
    await press("Перенести выбранные (1)");
    await until(() => text().includes("Перенесено 1 файл"), () => `итог переноса; в окне: ${dialog()?.innerHTML.replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").slice(0, 400)}; кнопки: ${[...document.querySelectorAll("button")].map((b) => b.textContent.trim()).filter(Boolean).join("|")}`, 60000);
    checkElderlyRules("мастер: итог");
    await press("Готово");
    await until(() => !dialog(), "мастер закрыт");
    // Второй раз — остался один новый снимок, он и переносится.
    await click("Перенести с телефона");
    await until(() => text().includes("Выбрано 1 из 1"), "второй раз: «Выбрано 1 из 1»");
    expect(text().includes("Ещё 1 файл уже перенесено раньше"), `нет упоминания перенесённого: ${dialog()?.innerText}`);
    await press("Перенести всё");
    await until(() => text().includes("Перенесено 1 файл"), "второй перенос", 60000);
    await press("Готово");
    await until(() => !dialog(), "мастер закрыт");
    // Третий раз — всё уже перенесено.
    await click("Перенести с телефона");
    await until(() => text().includes("Всё уже перенесено"), "повтор: «Всё уже перенесено»");
    await press("Готово");
    await until(() => !dialog(), "мастер закрыт");
  });

  await test("настройки: понятные слова и крупные цели", async () => {
    await click("Настройки");
    await until(() => text().includes("Крупный текст"), "окно настроек");
    expect(text().includes("Запоминать, что есть на телефоне"), "нет пункта «Запоминать, что есть на телефоне»");
    checkElderlyRules("настройки");
  });

  await report(failed ? `итог: не прошло ${failed}` : "итог: всё прошло");
  await invoke("dev_exit", { code: failed ? 1 : 0 });
})().catch(async (e) => {
  await window.__TAURI_INTERNALS__.invoke("dev_report", { line: `FAIL сценарий упал: ${e.message}` });
  await window.__TAURI_INTERNALS__.invoke("dev_exit", { code: 1 });
});
