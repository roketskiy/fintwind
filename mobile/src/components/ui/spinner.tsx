import { cn } from "cn";
import { LoaderIcon } from "@/components/icons";

function Spinner({
  className,
  ...props
}: React.ComponentProps<typeof LoaderIcon>) {
  return (
    <LoaderIcon
      data-slot="spinner"
      role="status"
      aria-hidden={false}
      aria-label="正在加载"
      className={cn("animate-spin", className)}
      {...props}
    />
  );
}

export { Spinner };
