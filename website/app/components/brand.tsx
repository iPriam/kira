type KiraWordmarkProps = {
  className?: string;
};

export function KiraWordmark({ className = "" }: KiraWordmarkProps) {
  return (
    <span
      role="img"
      aria-label="Kira"
      className={`inline-block shrink-0 bg-no-repeat ${className}`.trim()}
      style={{
        aspectRatio: "2379 / 1144",
        maxHeight: "1.2rem",
        maxWidth: "10rem",
        backgroundImage: "url('/KiraNameIcon.png')",
        backgroundPosition: "50.3% 50.3%",
        backgroundRepeat: "no-repeat",
        backgroundSize: "168.143% 349.65%",
      }}
    />
  );
}
