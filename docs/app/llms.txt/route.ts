import { llmsIndex } from '@/lib/source';

export const revalidate = false;

export function GET() {
  return new Response(llmsIndex());
}
