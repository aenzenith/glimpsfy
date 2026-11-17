<script lang="ts">
// The kit's <g-icon>: an outline icon from `src/lib/icons.ts` at 12, 14, 16, 20, 24, 32 or 40 px.
// It renders the kit's own `g-icon` element, so the kit's CSS sizes and places it. Without `label`
// the icon is decorative and hidden from assistive technology.
import { defineComponent, h } from "vue";

import { iconSvg } from "../lib/icons";

export default defineComponent({
  name: "GIcon",
  props: {
    name: { type: String, required: true },
    size: { type: [Number, String], default: 20 },
    label: { type: String, default: undefined },
  },
  setup(props) {
    return () =>
      h("g-icon", {
        size: String(props.size),
        "aria-hidden": props.label ? undefined : "true",
        innerHTML: iconSvg(props.name, Number(props.size) || 20, props.label),
      });
  },
});
</script>
