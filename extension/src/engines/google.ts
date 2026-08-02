import type {
  ExtractionFieldRule,
  ExtractionRules,
  SearchResult,
  SearchResultPayload
} from "../types";

export function buildGoogleExtractionExpression(
  rules: ExtractionRules,
  limit: number,
  waitMs: number,
  scrollToEnd: boolean
): string {
  return `(${configurableExtractor.toString()})(${JSON.stringify({
    rules,
    limit,
    waitMs,
    scrollToEnd
  })})`;
}

interface ExtractorOptions {
  rules: ExtractionRules;
  limit: number;
  waitMs: number;
  scrollToEnd: boolean;
}

async function configurableExtractor(
  options: ExtractorOptions
): Promise<SearchResultPayload> {
  const sleep = (milliseconds: number) =>
    new Promise<void>((resolve) => setTimeout(resolve, milliseconds));
  const cleanText = (value: string | null | undefined) =>
    (value ?? "").replace(/\s+/g, " ").trim();

  const absoluteUrl = (raw: string): string => {
    if (!raw.trim()) {
      return "";
    }
    try {
      const parsed = new URL(raw, location.href);
      if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
        return "";
      }
      return parsed.href;
    } catch {
      return "";
    }
  };

  const isGoogleSearchHost = (hostname: string) =>
    /(^|\.)google\.[a-z.]+$/i.test(hostname);

  const unwrapGoogleUrl = (raw: string): string => {
    try {
      const parsed = new URL(raw, location.href);
      if (!isGoogleSearchHost(parsed.hostname)) {
        return parsed.href;
      }
      if (parsed.pathname === "/url") {
        const target = parsed.searchParams.get("q") ?? parsed.searchParams.get("url");
        return target ? absoluteUrl(target) : "";
      }
      if (parsed.pathname === "/imgres") {
        const target =
          parsed.searchParams.get("imgrefurl") ?? parsed.searchParams.get("imgurl");
        return target ? absoluteUrl(target) : "";
      }
      return "";
    } catch {
      return "";
    }
  };

  const selectElement = (
    root: Element,
    fieldName: string,
    selectors: string[]
  ): Element | null => {
    for (const selector of selectors) {
      try {
        const element =
          selector === "&" || selector === ":scope"
            ? root
            : root.querySelector(selector);
        if (element) {
          return element;
        }
      } catch (error) {
        throw new Error(
          `Invalid selector for field ${fieldName}: ${selector}: ${
            error instanceof Error ? error.message : String(error)
          }`
        );
      }
    }
    return null;
  };

  const readField = (
    root: Element,
    fieldName: string,
    rule: ExtractionFieldRule
  ): string => {
    const element = selectElement(root, fieldName, rule.selectors);
    if (!element) {
      return "";
    }

    const attribute = rule.attribute ?? "text";
    let value = "";
    if (attribute === "text") {
      value = cleanText(element.textContent);
    } else if (attribute === "href") {
      value =
        (element as HTMLAnchorElement).href || element.getAttribute("href") || "";
      value = value.trim();
    } else if (attribute === "src") {
      const media = element as HTMLImageElement;
      value = media.currentSrc || media.src || element.getAttribute("src") || "";
      value = value.trim();
    } else {
      value = cleanText(element.getAttribute(attribute));
    }

    if (rule.transform === "absolute_url") {
      value = absoluteUrl(value);
    } else if (rule.transform === "google_url") {
      value = unwrapGoogleUrl(value);
    }
    if (rule.maxLength && value.length > rule.maxLength) {
      value = value.slice(0, rule.maxLength);
    }
    return value;
  };

  const extractResult = (
    root: Element,
    missingRequired: Record<string, number>
  ): SearchResult | null => {
    const result: SearchResult = {};
    for (const [fieldName, rule] of Object.entries(options.rules.fields)) {
      const value = readField(root, fieldName, rule);
      if (!value) {
        if (rule.required) {
          missingRequired[fieldName] = (missingRequired[fieldName] ?? 0) + 1;
          return null;
        }
        continue;
      }
      result[fieldName] = value;
    }
    return Object.keys(result).length > 0 ? result : null;
  };

  const collectRoots = (): Element[] => {
    const roots = new Set<Element>();
    for (const selector of options.rules.rootSelectors) {
      let matches: NodeListOf<Element>;
      try {
        matches = document.querySelectorAll(selector);
      } catch (error) {
        throw new Error(
          `Invalid root selector ${selector}: ${
            error instanceof Error ? error.message : String(error)
          }`
        );
      }
      for (const element of matches) {
        roots.add(element);
      }
    }
    return [...roots];
  };

  let lastDiagnostics: {
    rootCount: number;
    missingRequired: Record<string, number>;
  } = { rootCount: 0, missingRequired: {} };
  const rankedResults = new Map<
    string,
    { rank: number; result: SearchResult }
  >();
  const rootRanks = new WeakMap<Element, number>();
  let nextRootRank = 0;

  const collect = (): void => {
    const roots = collectRoots();
    const missingRequired: Record<string, number> = {};
    for (const root of roots) {
      let rank = rootRanks.get(root);
      if (rank === undefined) {
        rank = nextRootRank;
        nextRootRank += 1;
        rootRanks.set(root, rank);
      }
      const result = extractResult(root, missingRequired);
      if (!result) {
        continue;
      }
      const dedupeValue = result[options.rules.dedupeField] ?? JSON.stringify(result);
      const existing = rankedResults.get(dedupeValue);
      if (!existing || rank <= existing.rank) {
        rankedResults.set(dedupeValue, { rank, result });
      }
    }
    lastDiagnostics = { rootCount: roots.length, missingRequired };
  };

  const orderedResults = (): SearchResult[] =>
    [...rankedResults.values()]
      .sort((left, right) => left.rank - right.rank)
      .slice(0, options.limit)
      .map(({ result }) => result);

  const detectInterruption = () => {
    if (
      location.pathname.startsWith("/sorry/") ||
      document.querySelector("#captcha-form, form[action*='/sorry/']")
    ) {
      throw new Error("Google returned a verification page");
    }
    if (location.hostname.startsWith("consent.google.")) {
      throw new Error("Google returned a consent page");
    }
  };

  const deadline = Date.now() + options.waitMs;
  collect();
  while (
    rankedResults.size === 0 &&
    lastDiagnostics.rootCount === 0 &&
    Date.now() < deadline
  ) {
    detectInterruption();
    await sleep(Math.min(200, Math.max(0, deadline - Date.now())));
    collect();
  }

  if (options.scrollToEnd) {
    const scrollHeight = () =>
      Math.max(
        document.documentElement.scrollHeight,
        document.body?.scrollHeight ?? 0
      );
    const scrollTop = () =>
      Math.max(
        window.scrollY,
        document.documentElement.scrollTop,
        document.body?.scrollTop ?? 0
      );
    let stableBottomChecks = 0;
    let noProgressChecks = 0;

    while (Date.now() < deadline) {
      detectInterruption();
      const viewportHeight = Math.max(
        window.innerHeight,
        document.documentElement.clientHeight,
        1
      );
      const beforeHeight = scrollHeight();
      const beforeTop = scrollTop();
      const targetTop = Math.min(
        beforeTop + Math.max(600, Math.floor(viewportHeight * 0.8)),
        Math.max(0, beforeHeight - viewportHeight)
      );
      window.scrollTo(0, targetTop);

      await sleep(Math.min(250, Math.max(0, deadline - Date.now())));
      detectInterruption();
      collect();

      const afterHeight = scrollHeight();
      const afterTop = scrollTop();
      const heightGrew = afterHeight > beforeHeight + 1;
      const scrollMoved = afterTop > beforeTop + 1;
      const atBottom = afterTop + viewportHeight >= afterHeight - 1;

      stableBottomChecks = atBottom && !heightGrew ? stableBottomChecks + 1 : 0;
      noProgressChecks =
        !heightGrew && !scrollMoved ? noProgressChecks + 1 : 0;
      if (stableBottomChecks >= 4 || noProgressChecks >= 4) {
        break;
      }
    }
  } else {
    while (rankedResults.size === 0 && Date.now() < deadline) {
      detectInterruption();
      await sleep(Math.min(200, Math.max(0, deadline - Date.now())));
      collect();
    }
  }
  detectInterruption();

  const results = orderedResults();
  if (results.length === 0) {
    const pageText = cleanText(document.body?.textContent);
    if (/did not match any documents|no results found/i.test(pageText)) {
      return { results: [] };
    }
    if (lastDiagnostics.rootCount > 0) {
      const missing = Object.entries(lastDiagnostics.missingRequired)
        .map(([field, count]) => `${field}=${count}`)
        .join(", ");
      throw new Error(
        `Configured roots matched ${lastDiagnostics.rootCount} elements, but no result passed required fields${
          missing ? `: ${missing}` : ""
        }`
      );
    }
    throw new Error("No configured search result containers were found");
  }
  return { results };
}
