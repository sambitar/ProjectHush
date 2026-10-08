import {
  createElement,
  Lock,
  MessageCircle,
  Mail,
  Folder,
  Shield,
  Settings,
  Plus,
  Search,
  Send,
  Paperclip,
  Smile,
  MoreHorizontal,
  Heart,
  ThumbsUp,
  Laugh,
  Download,
  Moon,
  Sun,
  Monitor,
  Inbox,
  Clock,
  FileText,
  Star,
  Archive,
  Trash2,
  Reply,
  ChevronLeft,
  X,
  type IconNode,
} from "lucide";

const ICONS: Record<string, IconNode> = {
  lock: Lock,
  chats: MessageCircle,
  mail: Mail,
  files: Folder,
  vault: Shield,
  settings: Settings,
  plus: Plus,
  search: Search,
  send: Send,
  attach: Paperclip,
  smile: Smile,
  more: MoreHorizontal,
  heart: Heart,
  thumbsUp: ThumbsUp,
  laugh: Laugh,
  download: Download,
  moon: Moon,
  sun: Sun,
  monitor: Monitor,
  inbox: Inbox,
  outbox: Clock,
  drafts: FileText,
  star: Star,
  archive: Archive,
  trash: Trash2,
  reply: Reply,
  back: ChevronLeft,
  close: X,
};

export type IconName = keyof typeof ICONS;

export function icon(name: IconName): SVGElement {
  const el = createElement(ICONS[name]);
  el.setAttribute("width", "20");
  el.setAttribute("height", "20");
  el.setAttribute("stroke-width", "1.75");
  el.setAttribute("aria-hidden", "true");
  return el;
}
