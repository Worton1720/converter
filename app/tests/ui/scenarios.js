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

  await test("сценарий 1: фото с телефона → JPEG", async () => {
    const added = await invoke("add_paths", { paths: [`${data}/phone`] });
    expect(added === 2, `добавлено ${added} файлов вместо 2`);
    await until(() => store().order.length === 2, "файлы в списке");
    checkElderlyRules("список файлов");
    await click("Начать");
    await until(
      () => store().order.every((p) => store().rows[p].job.status === "done"),
      "все файлы готовы",
      90000,
    );
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

  await test("настройки: понятные слова и крупные цели", async () => {
    await click("Настройки");
    await until(() => text().includes("Крупный текст"), "окно настроек");
    checkElderlyRules("настройки");
  });

  await report(failed ? `итог: не прошло ${failed}` : "итог: всё прошло");
  await invoke("dev_exit", { code: failed ? 1 : 0 });
})().catch(async (e) => {
  await window.__TAURI_INTERNALS__.invoke("dev_report", { line: `FAIL сценарий упал: ${e.message}` });
  await window.__TAURI_INTERNALS__.invoke("dev_exit", { code: 1 });
});
