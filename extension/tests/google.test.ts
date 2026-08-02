import { buildGoogleExtractionExpression } from "../src/engines/google";
import type { ExtractionRules, SearchResultPayload } from "../src/types";

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) {
    throw new Error(message);
  }
}

const rules: ExtractionRules = {
  rootSelectors: ["[data-result]"],
  dedupeField: "url",
  fields: {
    url: {
      selectors: ["a"],
      attribute: "href",
      transform: "google_url",
      required: true,
      maxLength: null
    },
    title: {
      selectors: ["h3"],
      attribute: null,
      transform: "none",
      required: true,
      maxLength: 200
    },
    imgurl: {
      selectors: ["[data-bla] > img"],
      attribute: "src",
      transform: "absolute_url",
      required: true,
      maxLength: null
    }
  }
};

const expression = buildGoogleExtractionExpression(rules, 10, 5_000, true);
assert(expression.includes("data-result"), "root selector is missing");
assert(expression.includes('"dedupeField":"url"'), "dedupe field is missing");
assert(expression.includes('"attribute":"href"'), "field attribute is missing");
assert(expression.includes('"attribute":"src"'), "image source attribute is missing");
assert(expression.includes('"maxLength":200'), "field max length is missing");
assert(expression.includes('"limit":10'), "result limit is missing");
assert(expression.includes('"transform":"absolute_url"'), "image URL transform is missing");
assert(expression.includes('"scrollToEnd":true'), "scroll option is missing");
assert(expression.includes("window.scrollTo"), "scroll implementation is missing");

function installPage(imageSource: string): void {
  const link = {
    href: "https://example.com/tokyo",
    getAttribute: (name: string) =>
      name === "href" ? "https://example.com/tokyo" : null
  };
  const heading = {
    textContent: "Tokyo",
    getAttribute: () => null
  };
  const image = {
    currentSrc: imageSource,
    src: imageSource,
    getAttribute: (name: string) => (name === "src" ? imageSource : null)
  };
  const root = {
    querySelector: (selector: string) => {
      if (selector === "a") {
        return link;
      }
      if (selector === "h3") {
        return heading;
      }
      if (selector === "[data-bla] > img") {
        return image;
      }
      return null;
    }
  };

  Object.defineProperty(globalThis, "location", {
    configurable: true,
    value: {
      href: "https://www.google.com/search?q=Tokyo&udm=2",
      hostname: "www.google.com",
      pathname: "/search"
    }
  });
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    value: {
      body: { textContent: "" },
      querySelector: () => null,
      querySelectorAll: () => [root]
    }
  });
}

async function extract(imageSource: string): Promise<SearchResultPayload> {
  installPage(imageSource);
  return (await globalThis.eval(
    buildGoogleExtractionExpression(rules, 10, 0, false)
  )) as SearchResultPayload;
}

const snapshotUrl = "https://encrypted-tbn0.gstatic.com/images?q=snapshot";
const snapshotResult = await extract(snapshotUrl);
assert(
  snapshotResult.results[0]?.imgurl === snapshotUrl,
  "HTTP snapshot image was not retained"
);

let rejectedBase64 = false;
try {
  await extract("data:image/jpeg;base64,/9j/4AAQSkZJRgABAQ");
} catch (error) {
  rejectedBase64 = String(error).includes("imgurl=1");
}
assert(rejectedBase64, "base64 image result was not rejected");

{
  let firstImageLoaded = false;
  let pageHeight = 1_600;
  let scrollPosition = 0;
  let scrollCalls = 0;
  const roots = Array.from({ length: 3 }, (_, index) => {
    const suffix = index + 1;
    const link = {
      href: `https://example.com/tokyo-${suffix}`,
      getAttribute: (name: string) =>
        name === "href" ? `https://example.com/tokyo-${suffix}` : null
    };
    const heading = {
      textContent: `Tokyo ${suffix}`,
      getAttribute: () => null
    };
    const imageSource = () =>
      index === 0 && !firstImageLoaded
        ? "data:image/jpeg;base64,/9j/4AAQSkZJRgABAQ"
        : `https://encrypted-tbn0.gstatic.com/images?q=${suffix}`;
    const image = {
      get currentSrc() {
        return imageSource();
      },
      get src() {
        return imageSource();
      },
      getAttribute: (name: string) => (name === "src" ? imageSource() : null)
    };
    return {
      querySelector: (selector: string) => {
        if (selector === "a") {
          return link;
        }
        if (selector === "h3") {
          return heading;
        }
        if (selector === "[data-bla] > img") {
          return image;
        }
        return null;
      }
    };
  });

  Object.defineProperty(globalThis, "location", {
    configurable: true,
    value: {
      href: "https://www.google.com/search?q=Tokyo&udm=2",
      hostname: "www.google.com",
      pathname: "/search"
    }
  });
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    value: {
      body: {
        textContent: "",
        get scrollHeight() {
          return pageHeight;
        },
        scrollTop: 0
      },
      documentElement: {
        clientHeight: 800,
        get scrollHeight() {
          return pageHeight;
        },
        scrollTop: 0
      },
      querySelector: () => null,
      querySelectorAll: () => roots
    }
  });
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      innerHeight: 800,
      get scrollY() {
        return scrollPosition;
      },
      scrollTo: (_left: number, top: number) => {
        scrollCalls += 1;
        scrollPosition = top;
        if (!firstImageLoaded) {
          firstImageLoaded = true;
          pageHeight += 800;
        }
      }
    }
  });

  const originalSetTimeout = globalThis.setTimeout;
  Object.defineProperty(globalThis, "setTimeout", {
    configurable: true,
    value: (callback: () => void) => {
      callback();
      return 0;
    }
  });
  try {
    const scrolledResult = (await globalThis.eval(
      buildGoogleExtractionExpression(rules, 2, 5_000, true)
    )) as SearchResultPayload;
    assert(
      scrolledResult.results.length === 2,
      "result limit was not applied after scrolling"
    );
    assert(
      scrolledResult.results[0]?.url === "https://example.com/tokyo-1",
      "late-loaded leading result lost its DOM rank"
    );
    assert(
      scrolledResult.results[1]?.url === "https://example.com/tokyo-2",
      "results were not returned in DOM rank order"
    );
    assert(scrollCalls > 1, "page was not scrolled progressively");
    assert(scrollCalls < 10, "scrolling did not stop at the stable bottom");
  } finally {
    Object.defineProperty(globalThis, "setTimeout", {
      configurable: true,
      value: originalSetTimeout
    });
  }
}
