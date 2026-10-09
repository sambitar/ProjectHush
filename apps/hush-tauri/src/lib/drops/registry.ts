/**
 * The Drop registry.
 *
 * This is the single place a Drop is defined. The UI is a view over it, so
 * adding an organisation is one entry here and no UI change.
 *
 * ## Adding a real Drop
 *
 * 1. Find the organisation in the SecureDrop directory
 *    ({@link SECUREDROP_DIRECTORY_URL}) or on the organisation's own site.
 * 2. Copy the v3 `.onion` address exactly. Do not guess, do not accept one from
 *    a chat message, an email, or a server response.
 * 3. Set `onionAddress`, `verificationSource`, and `verified: true`.
 *
 * Until step 3 is done, leave `onionAddress: null` and `verified: false`. An
 * entry without a verified address is shown as unverified and cannot be opened.
 * **No onion address is invented here.** Addresses are only ever copied from a
 * source we trust.
 *
 * A verified address says the address is authentic, not that the service is up.
 * Al Jazeera's instance has been offline for an extended period and has been
 * withdrawn from the SecureDrop directory; its address is still the one the
 * organisation publishes, so it stays here rather than being guessed at or
 * silently dropped.
 *
 * ## Where the list comes from
 *
 * The organisations below are the live entries of the SecureDrop directory
 * ({@link SECUREDROP_DIRECTORY_URL}), read from its API. Each onion was checked
 * for reachability over Tor when it was added; `verified` still means only that
 * the address is authentic. A directory entry whose service did not answer is
 * left out rather than listed as dead.
 *
 * The scheme is whatever the instance serves. Most SecureDrop onions are plain
 * `http` — Tor authenticates the address and encrypts the link, so there is no
 * TLS layer. A few (the New York Times, the Washington Post) serve `https` and
 * redirect `http` to it. Both are accepted, and the window follows the redirect.
 */

import type { Drop } from "./types";
import { SECUREDROP_DIRECTORY_URL } from "./verification";

/** Port the development mock listens on; see `scripts/drops-dev-mock.sh`. */
export const DEV_MOCK_URL = "http://127.0.0.1:8791";

/** Al Jazeera's own tips page, which publishes its SecureDrop onion address. */
export const AL_JAZEERA_TIPS_URL = "https://www.aljazeera.com/tips";

const DROPS: Drop[] = [
  // ------------------------------------------------------------- real entries
  // Shown by default once verified. An entry still awaiting a verified address
  // stays here with `onionAddress: null` and is hidden unless the user turns on
  // "show unverified".
  {
    id: "aftenposten-as",
    name: "Aftenposten",
    description:
      "Norwegian newspaper accepting confidential tips from sources.",
    organization: "Aftenposten AS",
    category: "News",
    country: "Norway",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://tiykfvhb562gheutfnedysnhrxpxoztyszkqyroloyepwzxmxien77id.onion/",
    website: "https://www.aftenposten.no/securedrop",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "aftonbladet",
    name: "Aftonbladet",
    description:
      "Swedish evening newspaper accepting confidential tips through SecureDrop.",
    organization: "Aftonbladet",
    category: "News",
    country: "Sweden",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://xm33ge4kupk5o66eqxcd2r4fqcplpqb2sbdduf5z2nw4g2jrxe57luid.onion/",
    website: "https://www.aftonbladet.se/securedrop/",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "al-jazeera",
    name: "Al Jazeera",
    description:
      "Anonymous submission channel for the Al Jazeera media network.",
    organization: "Al Jazeera",
    category: "News",
    country: "International",
    protocol: "SecureDrop",
    // Published on Al Jazeera's own tips page, which is the source of record
    // for this address. Copied verbatim, http included: SecureDrop serves its
    // onion over http and Tor provides the encryption.
    onionAddress:
      "http://ivoj4zpse4n23bb4efvrzwsaxa3vojszviilhyjyeyafxnoupsnogaqd.onion/",
    website: "https://www.aljazeera.com/tips",
    verificationSource: AL_JAZEERA_TIPS_URL,
    verified: true,
  },
  {
    id: "bloomberg-industry-group",
    name: "Bloomberg Industry Group",
    description:
      "Legal, regulatory and government news outlet accepting confidential tips.",
    organization: "Bloomberg Industry Group",
    category: "News",
    country: "United States",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://3qqupamfznjrai6p4td7epa6pvidpumas2xjsihat73z23f37l2y52id.onion/",
    website: "https://news.bloomberglaw.com/tips",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "bloomberg-news",
    name: "Bloomberg News",
    description:
      "International news agency accepting confidential tips through SecureDrop.",
    organization: "Bloomberg News",
    category: "News",
    country: "International",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://ogdwaroarq4p6rnfn2hl4crvldyruyc2g24435qtxmd3twhevg7dsqid.onion/",
    website: "https://www.bloomberg.com/tips/",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "cbc",
    name: "CBC",
    description:
      "Canada's public broadcaster, accepting confidential submissions from sources.",
    organization: "CBC",
    category: "News",
    country: "Canada",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://gppg43zz5d2yfuom3yfmxnnokn3zj4mekt55onlng3zs653ty4fio6qd.onion/",
    website: "https://www.cbc.ca/securedrop",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "dagbladet",
    name: "Dagbladet",
    description:
      "Norwegian daily newspaper accepting confidential tips through SecureDrop.",
    organization: "Dagbladet",
    category: "News",
    country: "Norway",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://ydbpz5knb6ji3bdtahhm3wo7sed6lsy5vqnwfpnhpez4bquvoexbz7qd.onion/",
    website: "https://www.dagbladet.no/securedrop/",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "der-spiegel",
    name: "Der Spiegel",
    description:
      "German news magazine with a long-running SecureDrop for investigative tips.",
    organization: "Der Spiegel",
    category: "News",
    country: "Germany",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://q6vdlj2ukulrqk37piqgxucpcwtxzdjhvjzqrfbevuhrzimsgjltmpqd.onion/",
    website: "https://www.spiegel.de/extra/so-nehmen-informanten-sicheren-kontakt-zum-spiegel-auf-a-1030502.html",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "financial-times",
    name: "Financial Times",
    description:
      "Business newspaper accepting confidential tips through SecureDrop.",
    organization: "Financial Times",
    category: "News",
    country: "United Kingdom",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://nqu6crmtnzs2hs5abo2uqni53yqsnnwqnerdxuzyz5yxairxlzjzt6yd.onion/",
    website: "https://www.ft.com/news-tips/",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "forbes",
    name: "Forbes",
    description:
      "American business magazine accepting confidential tips through SecureDrop.",
    organization: "Forbes",
    category: "News",
    country: "United States",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://6zonlfhh7aqtfwoyvdlad3nxn6ljecx2k6tyyy3spt43nn54q6lvncid.onion/",
    website: "https://www.forbes.com/fdc/securedrop.html",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "forbidden-stories",
    name: "Forbidden Stories",
    description:
      "Consortium of journalists that continues and publishes the work of reporters under threat.",
    organization: "Forbidden Stories",
    category: "Investigative journalism",
    country: "International",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://fg25fqpu2dnxp24xs3jlcley4hp2inshpzek44q3czkhq3zffoqk26id.onion/",
    website: "https://forbiddenstories.org/safebox-network-tutorial/",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "greekleaks",
    name: "Greekleaks",
    description:
      "SecureDrop run by Reporters United, a Greek network of investigative journalists.",
    organization: "Reporters United",
    category: "Investigative journalism",
    country: "Greece",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://jatasaqcoe7lqdpcyxo7vl3e5tdvl5jgmtadfat77i25qdj6z6a4ulad.onion/",
    website: "https://reportersunited.gr/greekleaks/",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "new-york-times",
    name: "New York Times",
    description:
      "American newspaper accepting confidential tips through SecureDrop.",
    organization: "New York Times",
    category: "News",
    country: "United States",
    protocol: "SecureDrop",
    // This instance serves https and redirects http to it, so the window
    // follows a redirect. Measured through Tor; see docs/DESIGN.md.
    onionAddress: "https://ej3kv4ebuugcmuwxctx5ic7zxh73rnxt42soi3tdneu2c2em55thufqd.onion/",
    website: "https://www.nytimes.com/tips",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "news24",
    name: "News24",
    description:
      "South African news publisher accepting confidential tips through SecureDrop.",
    organization: "News24",
    category: "News",
    country: "South Africa",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://uhmj4j5pnwbpmkebfze3qgjmkum465fvok376nxtpku5yvyv5takz6qd.onion/",
    website: "https://www.news24.com/tips",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "nrk",
    name: "NRK",
    description:
      "Norway's public broadcaster, accepting confidential tips through SecureDrop.",
    organization: "NRK",
    category: "News",
    country: "Norway",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://537ztcntpbmspja4mkpxldpsoc46mqlssnsaklqnfw3gnlpj5glcjgid.onion/",
    website: "https://www.nrk.no/varsle/",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "propublica",
    name: "ProPublica",
    description:
      "Independent nonprofit newsroom producing investigative journalism.",
    organization: "ProPublica",
    category: "Investigative journalism",
    country: "United States",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://33xu4yhum2eiisxm6fntaslayop76fvaqgt3ak5dakdm3t7cub25cead.onion/",
    website: "https://propublica.org/tips",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "stefania-maurizi",
    name: "Stefania Maurizi",
    description:
      "Italian investigative journalist focused on leaks and whistleblowers.",
    organization: "Stefania Maurizi",
    category: "Investigative journalism",
    country: "Italy",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://jxsb4ovmavjy3r64bak4ha63xwggf3nzf3vikvs23r2avm5rhzmaqtqd.onion/",
    website: "https://stefaniamaurizi.it/en-contactme.html",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "süddeutsche-zeitung",
    name: "Süddeutsche Zeitung",
    description:
      "German daily newspaper accepting confidential tips through SecureDrop.",
    organization: "Süddeutsche Zeitung",
    category: "News",
    country: "Germany",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://udhauo3m3fh7v6yfiuornjzxn3fh6vlp4ooo3wogvghcnv5xik6mnayd.onion/",
    website: "https://www.sueddeutsche.de/projekte/kontakt/",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "the-economist",
    name: "The Economist",
    description:
      "International affairs newspaper accepting confidential tips from sources.",
    organization: "The Economist",
    category: "News",
    country: "International",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://mxmddqsh4jnr4gjan37ayin3fu5ecnejxge4wjhj4i45qq5djbxdjtad.onion/",
    website: "https://www.economist.com/securedrop/",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "globe-and-mail",
    name: "The Globe and Mail",
    description:
      "Canadian national newspaper accepting confidential tips through SecureDrop.",
    organization: "The Globe and Mail",
    category: "News",
    country: "Canada",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://a4zum5ydurvljrohxqp2rjjal5kro4ge2q2qizuonf2jubkhcr627gad.onion/",
    website: "https://sec.theglobeandmail.com/securedrop/",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "guardian",
    name: "The Guardian",
    description:
      "British daily newspaper accepting confidential tips through SecureDrop.",
    organization: "The Guardian",
    category: "News",
    country: "United Kingdom",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://xp44cagis447k3lpb4wwhcqukix6cgqokbuys24vmxmbzmaq2gjvc2yd.onion/",
    website: "https://www.theguardian.com/securedrop",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "washington-post",
    name: "The Washington Post",
    description:
      "American daily newspaper accepting confidential tips through SecureDrop.",
    organization: "The Washington Post",
    category: "News",
    country: "United States",
    protocol: "SecureDrop",
    // This instance serves https and redirects http to it, so the window
    // follows a redirect. Measured through Tor; see docs/DESIGN.md.
    onionAddress: "https://vfnmxpa6fo4jdpyq3yneqhglluweax2uclvxkytfpmpkp5rsl75ir5qd.onion/",
    website: "https://www.washingtonpost.com/securedrop",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "whistleblowco",
    name: "Whistleblow.co",
    description:
      "US whistleblower news and legal resource accepting confidential disclosures.",
    organization: "Whistleblow.co",
    category: "Whistleblowing",
    country: "United States",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://zuybtx2nop5cxfz4gyc7hzxpterf3pvf6zixe5cscajhlgqrexpkcoqd.onion/",
    website: "https://www.whistleblow.co/whistleblowco-securedrop/",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  {
    id: "whistleblower-aid",
    name: "Whistleblower Aid",
    description:
      "Nonprofit law firm representing whistleblowers; accepts disclosures through SecureDrop.",
    organization: "Whistleblower Aid",
    category: "Whistleblowing",
    country: "United States",
    protocol: "SecureDrop",
    // Copied verbatim from the SecureDrop directory entry for this
    // organisation; http because that is what SecureDrop serves.
    onionAddress: "http://kogbxf4ysay2qzozmg7ar45ijqmj2vxrwqa4upzqq2i7sqj7wv7wcdqd.onion/",
    website: "https://whistlebloweraid.org/become-a-whistleblower/securedrop/",
    verificationSource: SECUREDROP_DIRECTORY_URL,
    verified: true,
  },
  // ------------------------------------------------------------ development
  // A clearly-marked test entry. It is never routed through Tor and is only
  // shown when development Drops are enabled (HUSH_DROPS_DEV=1).
  {
    id: "example-dev",
    name: "Example SecureDrop",
    description:
      "Development-only test Drop. Opens a local mock, not a real organisation.",
    organization: "Hush development",
    category: "Test",
    country: "Local",
    protocol: "SecureDrop",
    onionAddress: null,
    website: null,
    verificationSource: null,
    verified: false,
    devOnly: true,
    devTarget: DEV_MOCK_URL,
  },
];

/** Every Drop in the registry, in registry order. */
export function allDrops(): Drop[] {
  return DROPS;
}

/** Look a Drop up by id. */
export function dropById(id: string): Drop | undefined {
  return DROPS.find((d) => d.id === id);
}

/**
 * The Drops a rail should render.
 *
 * By default only verified entries are shown. `includeUnverified` reveals the
 * real organisations whose address is still pending, and `includeDev` reveals
 * the test entry.
 */
export function visibleDrops(opts: {
  includeUnverified: boolean;
  includeDev: boolean;
}): Drop[] {
  return DROPS.filter((d) => {
    if (d.devOnly) return opts.includeDev;
    if (!opts.includeUnverified) return d.verified;
    return true;
  });
}

/** Count of entries that are verified and openable, for the header. */
export function verifiedCount(): number {
  return DROPS.filter((d) => d.verified).length;
}
